import { useCallback, useEffect, useRef, useState } from 'react';
import { PROFILE } from '../profile';
import { rpc } from '../ui/services';
import { runChecks, type Level, type Report } from './live';

export interface Sample {
  at: number;
  level: Level;
}

/**
 * Runs the live checks on request. `auto` repeats them on a timer; it is off until the reader turns
 * it on, because every run shows their address to the operator and the RPC providers.
 */
export function useReport(auto: boolean, everyMs = 30_000) {
  const [report, setReport] = useState<Report | null>(null);
  const [running, setRunning] = useState(false);
  const [history, setHistory] = useState<Sample[]>([]);
  const busy = useRef(false);
  const run = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    setRunning(true);
    try {
      const next = await runChecks(PROFILE, rpc);
      setReport(next);
      setHistory((h) => [...h, { at: next.at, level: next.overall }].slice(-24));
    } finally {
      busy.current = false;
      setRunning(false);
    }
  }, []);
  useEffect(() => {
    if (!auto) return;
    const t = setInterval(() => void run(), everyMs);
    return () => clearInterval(t);
  }, [auto, everyMs, run]);
  return { report, running, history, run };
}
