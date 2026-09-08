/**
 * Browser guest client for the zero-knowledge relay.
 *
 * Same protocol and crypto format as the Tauri app:
 *  - Room key in the URL fragment (`#k=`), never sent to the server.
 *  - Payloads are AES-256-GCM, base64url(iv || ciphertext).
 *
 * This client is guest-only: hosting (character card, history, LLM) lives in
 * the desktop app. The relay URL is simply the page's own origin, because the
 * Rust server serves these static files itself.
 */

export type ClosedReason =
  | ''
  | 'host_left'
  | 'expired'
  | 'not_found'
  | 'idle'
  | 'slow'
  | 'error'
  | 'left';

export type JoinError = '' | 'invalid_link' | 'missing_key';

interface MpMessageBase {
  id: string;
  ts: number;
}

export interface MpContentMessage extends MpMessageBase {
  kind: 'chat' | 'llm';
  author: string;
  text: string;
  streaming?: boolean;
  mine?: boolean;
}

export interface MpSystemMessage extends MpMessageBase {
  kind: 'system';
  event: 'participant_joined' | 'participant_left';
  count: number;
}

export type MpMessage = MpContentMessage | MpSystemMessage;

export interface SessionCharacter {
  name: string;
  prompt: string;
  greeting: string;
  initials: string;
  color: string;
  avatarUrl?: string;
}

export const mpState = $state({
  view: 'join' as 'join' | 'name' | 'room',
  connected: false,
  connecting: false,
  roomId: '',
  selfId: 0,
  count: 0,
  lockedBy: null as number | null,
  everyoneCanGenerate: false,
  messages: [] as MpMessage[],
  displayName: '',
  characterName: '',
  sessionCharacter: null as SessionCharacter | null,
  closedReason: '' as ClosedReason,
  error: '' as JoinError,
});

let ws: WebSocket | null = null;
let cryptoKey: CryptoKey | null = null;
let pending: { roomId: string; keyB64: string } | null = null;
const seenIds = new Set<string>();
const completedStreamIds = new Set<string>();

// ---------------------------------------------------------------- crypto

const b64u = {
  encode(buf: ArrayBuffer | Uint8Array): string {
    const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    let bin = '';
    for (const b of bytes) bin += String.fromCharCode(b);
    return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  },
  decode(s: string): Uint8Array {
    const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/'));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  },
};

async function importKey(b64: string): Promise<void> {
  const keyBytes = b64u.decode(b64) as Uint8Array<ArrayBuffer>;
  cryptoKey = await crypto.subtle.importKey('raw', keyBytes, { name: 'AES-GCM' }, false, [
    'encrypt',
    'decrypt',
  ]);
}

async function encryptJson(obj: unknown): Promise<string> {
  if (!cryptoKey) throw new Error('no room key');
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const plain = new TextEncoder().encode(JSON.stringify(obj));
  const cipher = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, cryptoKey, plain);
  const out = new Uint8Array(iv.length + cipher.byteLength);
  out.set(iv, 0);
  out.set(new Uint8Array(cipher), iv.length);
  return b64u.encode(out);
}

async function decryptJson(p: string): Promise<any | null> {
  if (!cryptoKey) return null;
  try {
    const raw = b64u.decode(p);
    const plain = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: raw.slice(0, 12) },
      cryptoKey,
      raw.slice(12),
    );
    return JSON.parse(new TextDecoder().decode(plain));
  } catch {
    return null; // wrong key or broken frame — drop silently
  }
}

// ---------------------------------------------------------------- entry

function parseLink(input: string): { roomId: string; keyB64: string } | null {
  try {
    const url = new URL(input, window.location.origin);
    const roomId = url.searchParams.get('mp') ?? '';
    const frag = new URLSearchParams(url.hash.replace(/^#/, ''));
    const key = frag.get('k') ?? '';
    if (!roomId || !key) return null;
    return { roomId: roomId.toUpperCase(), keyB64: key };
  } catch {
    return null;
  }
}

/** Call once on startup: opens a room shared via link and scrubs the URL. */
export function checkJoinLink(): void {
  const parsed = parseLink(window.location.href);
  if (!parsed) return;
  history.replaceState(null, '', window.location.pathname);
  pending = parsed;
  mpState.view = 'name';
}

/** Join screen: user pasted a link into the input field. */
export function submitLink(input: string): void {
  const trimmed = input.trim();
  const parsed = parseLink(trimmed);
  if (!parsed) {
    mpState.error = /^[A-Za-z0-9]{4,8}$/.test(trimmed) ? 'missing_key' : 'invalid_link';
    return;
  }
  mpState.error = '';
  pending = parsed;
  mpState.view = 'name';
}

/** Name screen: connect. */
export async function enterRoom(displayName: string): Promise<void> {
  if (!pending || mpState.connecting || mpState.connected) return;
  mpState.displayName = displayName.trim();
  mpState.connecting = true;
  mpState.closedReason = '';
  try {
    await importKey(pending.keyB64);
  } catch {
    mpState.connecting = false;
    mpState.error = 'invalid_link';
    mpState.view = 'join';
    return;
  }
  connect(pending.roomId);
}

export function backToJoin(): void {
  ws?.close(1000);
  ws = null;
  cryptoKey = null;
  pending = null;
  seenIds.clear();
  completedStreamIds.clear();
  Object.assign(mpState, {
    view: 'join',
    connected: false,
    connecting: false,
    roomId: '',
    selfId: 0,
    count: 0,
    lockedBy: null,
    everyoneCanGenerate: false,
    messages: [],
    characterName: '',
    sessionCharacter: null,
    closedReason: '',
    error: '',
  });
}

export function leaveRoom(): void {
  mpState.closedReason = 'left';
  ws?.close(1000);
  ws = null;
}

// ---------------------------------------------------------------- websocket

function connect(roomId: string): void {
  mpState.roomId = roomId;
  const wsBase = window.location.origin.replace(/^http/, 'ws');
  const socket = new WebSocket(`${wsBase}/ws/${roomId}`);
  let incomingFrameChain = Promise.resolve();
  ws = socket;

  socket.onopen = () => {
    ws?.send(JSON.stringify({ t: 'hello' }));
  };

  socket.onmessage = (ev) => {
    let msg: any;
    try {
      msg = JSON.parse(ev.data);
    } catch {
      return;
    }
    incomingFrameChain = incomingFrameChain
      .then(() => {
        if (ws !== socket) return;
        return handleServerMsg(msg);
      })
      .catch((error) => {
        console.error('Failed to process multiplayer frame', error);
      });
  };

  socket.onclose = (ev) => {
    const wasConnected = mpState.connected;
    mpState.connected = false;
    mpState.connecting = false;
    finalizeStreaming();
    if (!mpState.closedReason) {
      mpState.closedReason = mapCloseCode(ev.code, wasConnected);
    }
  };
}

function mapCloseCode(code: number, wasConnected: boolean): ClosedReason {
  switch (code) {
    case 4001: return 'host_left';
    case 4404: return 'not_found';
    case 4008: return 'idle';
    case 4413: return 'slow';
    case 1000: return wasConnected ? 'left' : 'error';
    default:   return 'error';
  }
}

async function handleServerMsg(msg: any): Promise<void> {
  switch (msg.t) {
    case 'welcome':
      mpState.connected = true;
      mpState.connecting = false;
      mpState.view = 'room';
      mpState.selfId = msg.you;
      mpState.count = msg.count;
      mpState.lockedBy = msg.locked_by ?? null;
      mpState.everyoneCanGenerate = msg.everyone_can_generate;
      break;

    case 'joined':
      mpState.count = msg.count;
      pushSystem('participant_joined', msg.count);
      break;

    case 'left':
      mpState.count = msg.count;
      pushSystem('participant_left', msg.count);
      break;

    case 'relay': {
      const inner = await decryptJson(msg.p);
      if (inner) handleDecrypted(inner);
      break;
    }

    case 'locked':
      mpState.lockedBy = msg.by;
      break;

    case 'unlocked':
      mpState.lockedBy = null;
      finalizeStreaming();
      break;

    case 'policy':
      mpState.everyoneCanGenerate = msg.everyone;
      break;

    case 'room_closed':
      mpState.closedReason = msg.reason === 'expired' ? 'expired' : 'host_left';
      break;
  }
}

// -------------------------------------------------- decrypted app frames

function applySessionCharacter(raw: unknown): void {
  if (!raw || typeof raw !== 'object') return;
  const candidate = raw as Partial<SessionCharacter>;
  if (typeof candidate.name !== 'string' || typeof candidate.prompt !== 'string') return;

  const avatarUrl = typeof candidate.avatarUrl === 'string'
    && (candidate.avatarUrl.startsWith('data:image/') || candidate.avatarUrl.startsWith('/'))
      ? candidate.avatarUrl
      : undefined;
  const character: SessionCharacter = {
    name: candidate.name,
    prompt: candidate.prompt,
    greeting: typeof candidate.greeting === 'string' ? candidate.greeting : '',
    initials: typeof candidate.initials === 'string' && candidate.initials
      ? candidate.initials
      : candidate.name.slice(0, 1).toUpperCase(),
    color: typeof candidate.color === 'string' ? candidate.color : 'bg-stone-700',
    ...(avatarUrl ? { avatarUrl } : {}),
  };

  mpState.sessionCharacter = character;
  mpState.characterName = character.name;
}

function insertSorted(message: MpMessage): void {
  mpState.messages.push(message);
  mpState.messages.sort((a, b) => a.ts - b.ts);
}

function handleDecrypted(inner: any): void {
  switch (inner.k) {
    case 'chat':
      if (typeof inner.id !== 'string' || seenIds.has(inner.id)) return;
      seenIds.add(inner.id);
      mpState.messages.push({
        id: inner.id,
        kind: 'chat',
        author: String(inner.name ?? '?'),
        text: String(inner.text ?? ''),
        ts: Number(inner.ts) || Date.now(),
      });
      break;

    case 'llm_d': {
      const mid = String(inner.mid);
      if (completedStreamIds.has(mid)) return;
      let m = mpState.messages.find(
        (message): message is MpContentMessage => message.kind === 'llm' && message.id === mid,
      );
      if (!m) {
        m = {
          id: mid,
          kind: 'llm',
          author: String(inner.name ?? mpState.characterName ?? 'AI'),
          text: '',
          ts: Number(inner.ts) || Date.now(),
          streaming: true,
        };
        seenIds.add(m.id);
        insertSorted(m);
        if (!mpState.characterName && inner.name) mpState.characterName = String(inner.name);
      }
      m.text += String(inner.d ?? '');
      break;
    }

    case 'llm_e': {
      const mid = String(inner.mid);
      if (inner.cancelled === true) {
        seenIds.add(mid);
        completedStreamIds.add(mid);
        mpState.messages = mpState.messages.filter((message) => message.id !== mid);
        break;
      }
      if (completedStreamIds.has(mid)) return;
      completedStreamIds.add(mid);
      const finalText = typeof inner.text === 'string' ? inner.text : null;
      const finalAuthor = typeof inner.name === 'string' ? inner.name : null;
      const finalTimestamp = Number(inner.ts) || 0;
      let m = mpState.messages.find(
        (message): message is MpContentMessage => message.kind === 'llm' && message.id === mid,
      );
      if (!m && finalText !== null) {
        m = {
          id: mid,
          kind: 'llm',
          author: finalAuthor ?? mpState.characterName ?? 'AI',
          text: finalText,
          ts: finalTimestamp || Date.now(),
        };
        seenIds.add(mid);
        insertSorted(m);
      } else if (m) {
        if (finalText !== null) m.text = finalText;
        if (finalAuthor !== null) m.author = finalAuthor;
        if (finalTimestamp) {
          m.ts = finalTimestamp;
          mpState.messages.sort((a, b) => a.ts - b.ts);
        }
      }
      if (m) {
        m.streaming = false;
        if (!m.text.trim()) {
          mpState.messages = mpState.messages.filter((message) => message.id !== mid);
        }
      }
      break;
    }

    case 'snap':
      if (typeof inner.charName === 'string' && !mpState.characterName) {
        mpState.characterName = inner.charName;
      }
      applySessionCharacter(inner.character);
      if (Array.isArray(inner.msgs)) {
        let added = false;
        for (const raw of inner.msgs) {
          if (typeof raw?.id !== 'string' || seenIds.has(raw.id)) continue;
          seenIds.add(raw.id);
          mpState.messages.push({
            id: raw.id,
            kind: raw.kind === 'llm' ? 'llm' : 'chat',
            author: String(raw.author ?? '?'),
            text: String(raw.text ?? ''),
            ts: Number(raw.ts) || 0,
          });
          added = true;
        }
        if (added) mpState.messages.sort((a, b) => a.ts - b.ts);
      }
      break;
  }
}

function pushSystem(event: MpSystemMessage['event'], count: number): void {
  mpState.messages.push({
    id: crypto.randomUUID(),
    kind: 'system',
    event,
    count,
    ts: Date.now(),
  });
}

function finalizeStreaming(): void {
  for (const message of mpState.messages) {
    if (message.kind === 'llm' && message.streaming) message.streaming = false;
  }
}

// ---------------------------------------------------------------- sending

export async function sendChat(text: string): Promise<void> {
  const trimmed = text.trim();
  if (!trimmed || mpState.lockedBy !== null) return;
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  const msg = {
    k: 'chat',
    id: crypto.randomUUID(),
    name: mpState.displayName || '?',
    text: trimmed,
    ts: Date.now(),
  };
  seenIds.add(msg.id);
  mpState.messages.push({
    id: msg.id,
    kind: 'chat',
    author: msg.name,
    text: msg.text,
    ts: msg.ts,
    mine: true,
  });
  ws.send(JSON.stringify({ t: 'relay', p: await encryptJson(msg) }));
}

export function requestGeneration(): void {
  if (mpState.lockedBy !== null || !mpState.everyoneCanGenerate) return;
  ws?.send(JSON.stringify({ t: 'gen_start' }));
}
