import { useEffect, useSyncExternalStore } from 'react';
import { due, noticeLevel, noticeText, remaining, watched } from '../leave/guard';
import { leaveStore } from '../leave/store';
import { isUnlocked } from '../crypto/wrap';
import { useWallet } from './services';

/**
 * Page-wide banners and the expiry guard's clock. The banners show the operator's emergency
 * notice and every open note that is within 14 days of its expiry. The guard runs while the app is
 * open: when the screen changes, when a wallet connects, and every minute.
 */
export function Watch({ screen, onClose }: { screen: string; onClose: () => void }) {
  const s = useSyncExternalStore(leaveStore.subscribe, () => leaveStore.state);
  const wallet = useWallet();

  useEffect(() => {
    void leaveStore.tick();
  }, [screen, wallet.connected]);

  useEffect(() => {
    const timer = setInterval(() => void leaveStore.tick(), 60_000);
    return () => clearInterval(timer);
  }, []);

  if (!isUnlocked()) return null;
  const advice = leaveStore.emergencyAdvice();
  const now = Math.floor(Date.now() / 1000);
  const near = s.rows.filter(
    (r) => watched(r.record) && noticeLevel(r.record.expiry!, now) !== undefined,
  );

  return (
    <>
      {advice && (
        <div className="warning" role="alert" data-testid="emergency-banner">
          <p>{advice}</p>
          {screen !== 'close' && (
            <button type="button" data-testid="emergency-go" onClick={onClose}>
              Open the Close screen
            </button>
          )}
        </div>
      )}
      {near.map(({ record, view }) => {
        const expiry = record.expiry!;
        const what = due(expiry, now);
        return (
          <div className="notice" key={record.id} data-testid="expiry-banner" data-due={what}>
            <p>{noticeText(view.noteId, noticeLevel(expiry, now)!, remaining(expiry, now))}</p>
            {screen !== 'close' && (
              <button type="button" onClick={onClose}>
                Open the Close screen
              </button>
            )}
          </div>
        );
      })}
    </>
  );
}
