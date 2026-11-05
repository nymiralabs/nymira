import { describe, expect, it, vi } from 'vitest';
import { PopupHandoff, type PopupHost } from '../src/wallet/popup';

// A window that records what is done to the popup it opens.
function fake() {
  const popup = { closed: false, location: { href: 'about:blank' }, close: vi.fn() } as unknown as {
    closed: boolean;
    location: { href: string };
    close: () => void;
  };
  popup.close = vi.fn(() => {
    popup.closed = true;
  });
  const native = vi.fn((url?: string): unknown =>
    url === 'about:blank' ? popup : { native: true },
  );
  const host = { open: native } as unknown as PopupHost;
  const handoff = new PopupHandoff(host, native as unknown as PopupHost['open']);
  return { host, native, popup, handoff };
}

describe('popup hand-off', () => {
  it('opens a blank window at once, and gives it to the next open call', () => {
    const { host, native, popup, handoff } = fake();
    handoff.arm();
    expect(native).toHaveBeenCalledTimes(1);
    expect(native).toHaveBeenCalledWith('about:blank', '_blank', expect.any(String));
    const got = host.open('https://wallet.example/approve', '_blank', 'width=1');
    expect(got).toBe(popup);
    expect(popup.location.href).toBe('https://wallet.example/approve');
    expect(native).toHaveBeenCalledTimes(1);
  });

  it('serves only the first open call from the pre-opened window', () => {
    const { host, native, handoff } = fake();
    handoff.arm();
    host.open('https://wallet.example/a');
    host.open('https://wallet.example/b', '_blank', 'x');
    expect(native).toHaveBeenCalledTimes(2);
    expect(native).toHaveBeenLastCalledWith('https://wallet.example/b', '_blank', 'x');
  });

  it('closes a window nobody used when the request ends', () => {
    const { popup, handoff } = fake();
    handoff.arm();
    handoff.release();
    expect(popup.close).toHaveBeenCalledTimes(1);
  });

  it('leaves a window alone that the wallet took', () => {
    const { host, popup, handoff } = fake();
    handoff.arm();
    host.open('https://wallet.example/a');
    handoff.release();
    expect(popup.close).not.toHaveBeenCalled();
  });

  it('is armed once: a second arm reuses the open window', () => {
    const { native, handoff } = fake();
    handoff.arm();
    handoff.arm();
    expect(native).toHaveBeenCalledTimes(1);
  });

  it('does nothing when the browser blocks the pre-open', () => {
    const { host, native, handoff } = fake();
    native.mockReturnValueOnce(null);
    handoff.arm();
    host.open('https://wallet.example/a');
    expect(native).toHaveBeenCalledTimes(2);
    handoff.release();
  });

  it('does not hand over a window the user already closed', () => {
    const { host, native, popup, handoff } = fake();
    handoff.arm();
    popup.closed = true;
    host.open('https://wallet.example/a');
    expect(native).toHaveBeenCalledTimes(2);
  });

  it('closes an armed window by itself after the lifetime', () => {
    vi.useFakeTimers();
    const { popup, handoff } = fake();
    handoff.arm(1000);
    vi.advanceTimersByTime(1001);
    expect(popup.close).toHaveBeenCalledTimes(1);
    handoff.release();
    expect(popup.close).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});
