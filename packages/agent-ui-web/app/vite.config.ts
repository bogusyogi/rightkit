import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const daemonTarget = process.env.RIGHTKIT_DAEMON_URL ?? 'http://127.0.0.1:3000'
const daemonPrefixes = [
  '/commands',
  '/health',
  '/mcp',
  '/messages',
  '/permission-rules',
  '/plugins',
  '/runtime',
  '/schedules',
  '/sessions',
  '/settings',
  '/skills',
  '/tasks',
  '/teams',
]

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'dist',
    // Inline small assets; the built UI should be a clean static bundle.
    assetsInlineLimit: 64 * 1024,
  },
  server: {
    // In dev, proxy daemon API calls so we don't need CORS headers.
    proxy: Object.fromEntries(
      daemonPrefixes.map((prefix) => [
        prefix,
        { target: daemonTarget, changeOrigin: true },
      ]),
    ),
  }
})
