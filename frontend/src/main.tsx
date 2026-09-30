import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import '@fontsource/space-grotesk/500.css'
import '@fontsource/space-grotesk/700.css'
import '@fontsource/jetbrains-mono/400.css'
import '@fontsource/jetbrains-mono/500.css'
import App from './App'
import { LightboxProvider } from './components/Lightbox'
import './styles.css'
import './styles/trace.css'
import './styles/files.css'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <LightboxProvider>
      <App />
    </LightboxProvider>
  </StrictMode>,
)
