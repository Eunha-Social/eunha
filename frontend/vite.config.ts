import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The Rust server (axum) serves the built SPA from `frontend/dist` and proxies
// nothing itself — so in dev we proxy the C2S API and OAuth endpoints to the
// running eunha server on :3000.
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'dist',
  },
  server: {
    proxy: {
      '/api': 'http://localhost:3000',
      '/oauth': 'http://localhost:3000',
      '/.well-known': 'http://localhost:3000',
    },
  },
})
