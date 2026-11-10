import { StrictMode, useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Site } from './Site';
import { AppConsole, type Tab } from './Console';
import './site.css';
import './console.css';

function Preview() {
  const [view, setView] = useState<'site' | Tab>('site');
  const [theme, setTheme] = useState<'dark' | 'light'>('dark');
  const flipTheme = () => setTheme((current) => (current === 'dark' ? 'light' : 'dark'));
  useEffect(() => {
    document.documentElement.style.backgroundColor = theme === 'dark' ? '#000' : '#fff';
    document.documentElement.style.colorScheme = theme;
  }, [theme]);
  if (view === 'site') {
    return (
      <Site
        theme={theme}
        onTheme={flipTheme}
        onOpenApp={() => {
          setView('chat');
          window.scrollTo({ top: 0, behavior: 'auto' });
        }}
      />
    );
  }
  return (
    <div className="nm-site" data-theme={theme}>
      <AppConsole
        tab={view}
        onTab={setView}
        onExit={() => setView('site')}
        theme={theme}
        onTheme={flipTheme}
      />
    </div>
  );
}

const root = document.getElementById('root');
if (!root) throw new Error('no #root element');

createRoot(root).render(
  <StrictMode>
    <Preview />
  </StrictMode>,
);
