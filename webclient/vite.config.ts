import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'
import { paraglideVitePlugin } from '@inlang/paraglide-js'

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    paraglideVitePlugin({
      project: './project.inlang',
      outdir: './src/paraglide',
      emitTsDeclarations: true,
      disableAsyncLocalStorage: true,
      strategy: ['localStorage', 'preferredLanguage', 'baseLocale'],
    }),
    svelte(),
  ],
  build: {
    outDir: '../web',
    emptyOutDir: true,
  },
})
