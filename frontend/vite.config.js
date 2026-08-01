import { fileURLToPath, URL } from 'node:url'
import { execSync } from 'node:child_process'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'

/** The backend's default port. phos holds 33000; picweight holds 33100. */
const BACKEND = process.env.PICWEIGHT_BACKEND || 'http://localhost:33100'

function getVersion() {
  if (process.env.PICWEIGHT_VERSION) return process.env.PICWEIGHT_VERSION
  try {
    return execSync('git describe --tags --exact-match', { encoding: 'utf-8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
  } catch {}
  try {
    return execSync('git symbolic-ref --short HEAD', { encoding: 'utf-8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
  } catch {}
  try {
    return 'sha-' + execSync('git rev-parse --short HEAD', { encoding: 'utf-8', stdio: ['ignore', 'pipe', 'ignore'] }).trim()
  } catch {}
  return 'unknown'
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    vue(),
    tailwindcss(),
  ],
  define: {
    __PICWEIGHT_VERSION__: JSON.stringify(getVersion()),
  },
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    proxy: {
      // Everything the SPA talks to lives behind the backend in production;
      // in dev the same paths proxy to it so cookies and redirects behave
      // identically to the deployed single-container setup.
      '/api': { target: BACKEND, changeOrigin: true },
      '/healthz': { target: BACKEND, changeOrigin: true },
      '/picweight.apk': { target: BACKEND, changeOrigin: true },
    },
  },
  css: {
    transformer: 'lightningcss',
  },
  build: {
    cssMinify: 'lightningcss',
  },
})
