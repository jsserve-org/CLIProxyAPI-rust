import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  base: '/console/',
  plugins: [react()],
  build: { rollupOptions: { output: { entryFileNames: 'assets/index.js', assetFileNames: (asset) => asset.name?.endsWith('.css') ? 'assets/index.css' : 'assets/[name][extname]' } } },
})
