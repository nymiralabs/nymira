import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './ui/App';
import './marketing/site.css';
import './marketing/showcase.css';
import './app.css';

const root = document.getElementById('root');
if (!root) throw new Error('no #root element');
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
