import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { Site } from './Site';
import './site.css';
import './console.css';

const root = document.getElementById('root');
if (!root) throw new Error('no #root element');

createRoot(root).render(
  <StrictMode>
    <Site />
  </StrictMode>,
);
