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

const TYPING_TIMEOUT_MS = 3000;
const TYPING_SEND_INTERVAL_MS = 1000;
const MAX_NAME_CHARS = 32;
const MAX_SNAPSHOT_AUTHOR_CHARS = 256;
const MAX_CHAT_CHARS = 4000;
const MAX_MESSAGE_ID_CHARS = 128;
const MAX_LLM_DELTA_CHARS = 64 * 1024;
const MAX_LLM_FINAL_CHARS = 512 * 1024;
const MAX_SNAPSHOT_MESSAGES = 1000;

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
  remoteTypingName: '',
  displayName: '',
  characterName: '',
  sessionCharacter: null as SessionCharacter | null,
  closedReason: '' as ClosedReason,
  error: '' as JoinError,
});

let ws: WebSocket | null = null;
let cryptoKey: CryptoKey | null = null;
let signingPublicKey: CryptoKey | null = null;
let guestToken = '';
let lastVerifiedHostSequence = 0;
let hostStateInitialized = false;
let pending: {
  roomId: string;
  keyB64: string;
  guestToken: string;
  signingPublicB64: string;
} | null = null;
const seenIds = new Set<string>();
const completedStreamIds = new Set<string>();
let remoteTypingId: number | null = null;
let remoteTypingTimer: ReturnType<typeof setTimeout> | null = null;
let localTyping = false;
let lastTypingSentAt = 0;
let typingRelayChain = Promise.resolve();

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
  if (keyBytes.byteLength !== 32) throw new Error('invalid room key');
  cryptoKey = await crypto.subtle.importKey('raw', keyBytes, { name: 'AES-GCM' }, false, [
    'encrypt',
    'decrypt',
  ]);
}

async function importSigningKey(b64: string): Promise<void> {
  const bytes = new Uint8Array(b64u.decode(b64));
  if (bytes.byteLength !== 65) throw new Error('invalid host signing key');
  signingPublicKey = await crypto.subtle.importKey(
    'raw',
    bytes,
    { name: 'ECDSA', namedCurve: 'P-256' },
    false,
    ['verify'],
  );
}

function hostSignatureInput(sequence: number, body: unknown): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(
    JSON.stringify({ v: 1, room: mpState.roomId, sequence, body }),
  ) as Uint8Array<ArrayBuffer>;
}

async function verifyHostEnvelope(envelope: any): Promise<unknown | null> {
  if (!signingPublicKey || envelope?.v !== 1 || !Number.isSafeInteger(envelope.sequence)) return null;
  if (envelope.sequence <= lastVerifiedHostSequence || typeof envelope.sig !== 'string') return null;
  try {
    const valid = await crypto.subtle.verify(
      { name: 'ECDSA', hash: 'SHA-256' },
      signingPublicKey,
      new Uint8Array(b64u.decode(envelope.sig)) as Uint8Array<ArrayBuffer>,
      hostSignatureInput(envelope.sequence, envelope.body),
    );
    if (!valid) return null;
    if (!hostStateInitialized && envelope.body?.k !== 'snap') return null;
    lastVerifiedHostSequence = envelope.sequence;
    hostStateInitialized = true;
    return envelope.body;
  } catch {
    return null;
  }
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

function parseLink(input: string): {
  roomId: string;
  keyB64: string;
  guestToken: string;
  signingPublicB64: string;
} | null {
  try {
    const url = new URL(input, window.location.origin);
    const roomId = url.searchParams.get('mp') ?? '';
    const frag = new URLSearchParams(url.hash.replace(/^#/, ''));
    const key = frag.get('k') ?? '';
    const guest = frag.get('g') ?? '';
    const signingPublic = frag.get('s') ?? '';
    if (!/^[A-HJ-NP-Z2-9]{6}$/i.test(roomId) || !key || !guest || !signingPublic) return null;
    return {
      roomId: roomId.toUpperCase(),
      keyB64: key,
      guestToken: guest,
      signingPublicB64: signingPublic,
    };
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
    await importSigningKey(pending.signingPublicB64);
    guestToken = pending.guestToken;
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
  signingPublicKey = null;
  guestToken = '';
  lastVerifiedHostSequence = 0;
  hostStateInitialized = false;
  pending = null;
  seenIds.clear();
  completedStreamIds.clear();
  clearRemoteTyping();
  localTyping = false;
  lastTypingSentAt = 0;
  typingRelayChain = Promise.resolve();
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
    remoteTypingName: '',
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
    ws?.send(JSON.stringify({ t: 'hello', guest_token: guestToken }));
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
      mpState.everyoneCanGenerate = false;
      break;

    case 'joined':
      mpState.count = msg.count;
      pushSystem('participant_joined', msg.count);
      break;

    case 'left':
      mpState.count = msg.count;
      clearRemoteTyping(Number(msg.id));
      pushSystem('participant_left', msg.count);
      break;

    case 'relay': {
      const inner = await decryptJson(msg.p);
      if (inner) await handleDecrypted(inner, Number(msg.from));
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
      // Policy displayed by the client is host-signed inside E2EE payloads.
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

function safeTimestamp(value: unknown, fallback = Date.now()): number {
  const timestamp = Number(value);
  return Number.isFinite(timestamp) && timestamp >= 0 && timestamp <= 8.64e15
    ? timestamp
    : fallback;
}

async function handleDecrypted(inner: any, sourceId: number): Promise<void> {
  if (!inner || typeof inner !== 'object') return;
  let hostAuthenticated = false;
  if (inner.k === 'host') {
    inner = await verifyHostEnvelope(inner);
    if (!inner || typeof inner !== 'object') return;
    hostAuthenticated = true;
  }
  if (['llm_d', 'llm_e', 'snap', 'policy'].includes(inner.k) && !hostAuthenticated) return;

  switch (inner.k) {
    case 'chat':
      if (typeof inner.id !== 'string'
        || inner.id.length > MAX_MESSAGE_ID_CHARS
        || seenIds.has(inner.id)
        || typeof inner.name !== 'string'
        || inner.name.length > MAX_NAME_CHARS
        || typeof inner.text !== 'string'
        || inner.text.length > MAX_CHAT_CHARS) return;
      seenIds.add(inner.id);
      mpState.messages.push({
        id: inner.id,
        kind: 'chat',
        author: String(inner.name ?? '?'),
        text: String(inner.text ?? ''),
        ts: safeTimestamp(inner.ts),
      });
      clearRemoteTyping(sourceId);
      break;

    case 'typing':
      if (sourceId === mpState.selfId) return;
      if (inner.active === false) {
        clearRemoteTyping(sourceId);
      } else {
        if (typeof inner.name === 'string' && inner.name.length <= MAX_NAME_CHARS) {
          showRemoteTyping(sourceId, inner.name);
        }
      }
      break;

    case 'llm_d': {
      const mid = String(inner.mid);
      if (!mid || mid.length > MAX_MESSAGE_ID_CHARS || typeof inner.d !== 'string'
        || inner.d.length > MAX_LLM_DELTA_CHARS) return;
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
          ts: safeTimestamp(inner.ts),
          streaming: true,
        };
        seenIds.add(m.id);
        insertSorted(m);
        if (!mpState.characterName && inner.name) mpState.characterName = String(inner.name);
      }
      m.text += inner.d;
      break;
    }

    case 'llm_e': {
      const mid = String(inner.mid);
      if (!mid || mid.length > MAX_MESSAGE_ID_CHARS) return;
      if (inner.cancelled === true) {
        seenIds.add(mid);
        completedStreamIds.add(mid);
        mpState.messages = mpState.messages.filter((message) => message.id !== mid);
        break;
      }
      if (completedStreamIds.has(mid)) return;
      completedStreamIds.add(mid);
      const finalText = typeof inner.text === 'string' && inner.text.length <= MAX_LLM_FINAL_CHARS
        ? inner.text
        : null;
      const finalAuthor = typeof inner.name === 'string' ? inner.name : null;
      const finalTimestamp = safeTimestamp(inner.ts, 0);
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
      if (Array.isArray(inner.msgs) && inner.msgs.length <= MAX_SNAPSHOT_MESSAGES) {
        let added = false;
        for (const raw of inner.msgs) {
          if (typeof raw?.id !== 'string'
            || raw.id.length > MAX_MESSAGE_ID_CHARS
            || seenIds.has(raw.id)
            || typeof raw.author !== 'string'
            || raw.author.length > MAX_SNAPSHOT_AUTHOR_CHARS
            || typeof raw.text !== 'string'
            || raw.text.length > MAX_LLM_FINAL_CHARS) continue;
          seenIds.add(raw.id);
          mpState.messages.push({
            id: raw.id,
            kind: raw.kind === 'llm' ? 'llm' : 'chat',
            author: String(raw.author ?? '?'),
            text: String(raw.text ?? ''),
            ts: safeTimestamp(raw.ts, 0),
          });
          added = true;
        }
        if (added) mpState.messages.sort((a, b) => a.ts - b.ts);
      }
      break;

    case 'policy':
      if (typeof inner.everyone === 'boolean') mpState.everyoneCanGenerate = inner.everyone;
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

function clearRemoteTyping(sourceId?: number): void {
  if (sourceId !== undefined && sourceId !== remoteTypingId) return;
  if (remoteTypingTimer) clearTimeout(remoteTypingTimer);
  remoteTypingTimer = null;
  remoteTypingId = null;
  mpState.remoteTypingName = '';
}

function showRemoteTyping(sourceId: number, name: string): void {
  clearRemoteTyping();
  remoteTypingId = sourceId;
  mpState.remoteTypingName = name;
  remoteTypingTimer = setTimeout(() => clearRemoteTyping(sourceId), TYPING_TIMEOUT_MS);
}

async function sendRelay(obj: unknown): Promise<void> {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(JSON.stringify({ t: 'relay', p: await encryptJson(obj) }));
}

export function sendTyping(active = true): void {
  if (!mpState.connected || !ws || ws.readyState !== WebSocket.OPEN) return;
  const now = Date.now();
  if (active && localTyping && now - lastTypingSentAt < TYPING_SEND_INTERVAL_MS) return;
  if (!active && !localTyping) return;

  localTyping = active;
  lastTypingSentAt = now;
  const frame = { k: 'typing', name: mpState.displayName || '?', active };
  typingRelayChain = typingRelayChain.then(() => sendRelay(frame)).catch(() => undefined);
}

export async function sendChat(text: string): Promise<void> {
  const trimmed = text.trim();
  if (!trimmed || mpState.lockedBy !== null) return;
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  sendTyping(false);
  await typingRelayChain;
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
  await sendRelay(msg);
}

export function requestGeneration(): void {
  if (mpState.lockedBy !== null || !mpState.everyoneCanGenerate) return;
  void sendRelay({ k: 'gen_req', id: crypto.randomUUID() });
}
