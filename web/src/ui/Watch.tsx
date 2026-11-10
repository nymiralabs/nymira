import { useEffect, useState, useSyncExternalStore } from 'react';
import { Button } from '../marketing/kit';
import { ExpiryBanner, ExpiryStack } from '../marketing/kit-close';
import { DAY, due, noticeLevel, noticeText, remaining, watched } from '../leave/guard';
import { leaveStore } from '../leave/store';
import { isUnlocked } from '../crypto/wrap';
import { useWallet } from './services';

/** The notice with its first clause in bold, as the console shows it. */
function NoticeLine({ text }: { text: string }) {
  const cut = text.indexOf(' (');
  const head = cut < 0 ? text : text.slice(0, cut);
  return (
    <>
      <strong>{head.charAt(0).toUpperCase() + head.slice(1)}</strong>
      {cut < 0 ? '' : <span className="nc-notice-rest">{text.slice(cut)}</span>}
    </>
  );
}

/**
 * Page-wide banners and the expiry guard's clock. The banners show the operator's emergency
 * notice and every open note that is within 14 days of its expiry. The guard runs while the app is
 * open: when the screen changes, when a wallet connects, and every minute.
 */
export function Watch({ screen, onClose }: { screen: string; onClose: () => void }) {
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();

  // The app unlocks on whichever screen asks for the passphrase; the guard starts when it does.
  const [unlocked, setUnlocked] = useState(isUnlocked());
  useEffect(() => {
    const timer = setInterval(() => setUnlocked(isUnlocked()), 500);
    return () => clearInterval(timer);
  }, []);

  useEffect(() => {
    void leaveStore.tick();
  }, [screen, wallet.connected, unlocked]);

  useEffect(() => {
    const timer = setInterval(() => void leaveStore.tick(), 60_000);
    return () => clearInterval(timer);
  }, []);

  if (!unlocked) return null;
  const advice = leaveStore.emergencyAdvice();
  const now = Math.floor(Date.now() / 1000);
  const near = s.rows.filter(
    (r) => watched(r.record) && noticeLevel(r.record.expiry!, now) !== undefined,
  );

  return (
    <>
      {advice && (
        <div className="nc-banner" role="alert" data-testid="emergency-banner">
          <span aria-hidden="true" />
          <p>{advice}</p>
          {screen !== 'close' && (
            <Button testId="emergency-go" onClick={onClose}>
              Open the Close screen
            </Button>
          )}
        </div>
      )}
      {near.length === 1 &&
        near.map(({ record, view }) => {
          const expiry = record.expiry!;
          return (
            <ExpiryBanner
              key={record.id}
              testId="expiry-banner"
              due={due(expiry, now)}
              left={Math.min(14, Math.floor(remaining(expiry, now) / DAY))}
              onOpen={screen !== 'close' ? onClose : undefined}
            >
              <NoticeLine
                text={noticeText(view.noteId, noticeLevel(expiry, now)!, remaining(expiry, now))}
              />
            </ExpiryBanner>
          );
        })}
      {near.length > 1 && (
        <ExpiryStack
          onOpen={screen !== 'close' ? onClose : undefined}
          items={near.map(({ record, view }) => {
            const expiry = record.expiry!;
            return {
              id: record.id,
              noteId: view.noteId,
              due: due(expiry, now),
              left: Math.min(14, Math.floor(remaining(expiry, now) / DAY)),
              level: noticeLevel(expiry, now),
              when:
                remaining(expiry, now) >= DAY
                  ? `expires in ${Math.floor(remaining(expiry, now) / DAY)} ${Math.floor(remaining(expiry, now) / DAY) === 1 ? 'day' : 'days'}`
                  : 'expires in less than a day',
              text: (
                <NoticeLine
                  text={noticeText(view.noteId, noticeLevel(expiry, now)!, remaining(expiry, now))}
                />
              ),
            };
          })}
        />
      )}
    </>
  );
}
