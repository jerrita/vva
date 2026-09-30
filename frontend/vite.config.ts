import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// 开发模式下 API 代理到本地 vva web 服务
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': process.env.VVA_API ?? 'http://127.0.0.1:3000',
    },
  },
})
