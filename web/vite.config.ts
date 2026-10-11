import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The Rust admin listener embeds exactly index.html, assets/index.js and
// assets/index.css, so the build keeps stable names and inlines the fonts.
export default defineConfig({
  base: '/console/',
  plugins: [react(), tailwindcss()],
  resolve: { alias: { '@': `${import.meta.dirname}/src` } },
  server: { proxy: { '/v0/management': 'http://127.0.0.1:8318' } },
  build: {
    assetsInlineLimit: 200_000,
    cssCodeSplit: false,
    chunkSizeWarningLimit: 1000,
    rollupOptions: {
      output: {
        entryFileNames: 'assets/index.js',
        assetFileNames: (asset) => (asset.names.some((name) => name.endsWith('.css')) ? 'assets/index.css' : 'assets/[name][extname]'),
      },
    },
  },
})
