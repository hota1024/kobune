import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// The port `kobune-studio` listens on by default.
const STUDIO = 'http://127.0.0.1:17823'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    proxy: {
      // `pnpm dev` serves the page from Vite but the API still comes from
      // the binary. The guard checks `Host` and `Origin` (see
      // `src/guard.rs`), and both would otherwise name Vite's port — so
      // the proxy rewrites them to the ones the studio expects. This
      // relaxes nothing on the server; it only makes the dev server look
      // like what it is proxying.
      '/api': {
        target: STUDIO,
        changeOrigin: true,
        headers: { Origin: STUDIO },
      },
    },
  },
})
