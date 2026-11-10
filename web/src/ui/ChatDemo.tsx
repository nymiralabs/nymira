// A suggested question plays a written answer here, without a note, a lease or a request. It is
// labelled as a sample the whole way through, so nobody takes it for a real attested answer.
import { useEffect, useRef, useState } from 'react';
import { Icon } from '../marketing/Icon';
import { CHECK_ROWS, Chip, Seal } from '../marketing/kit';

export const SUGGESTIONS = [
  'Explain how a prepaid note works.',
  'What does the operator see?',
  'How do I get my balance back?',
] as const;

const ANSWERS: Record<(typeof SUGGESTIONS)[number], string> = {
  'Explain how a prepaid note works.':
    'A prepaid note is credit you fund once with USDC. Your device proves it can pay for a lease without saying which deposit it came from, so the operator sees a proof and a size, not your account.',
  'What does the operator see?':
    'The operator relays ciphertext. It sees which model you use, the size and timing of each message and, in this web app, your IP address. It does not see the text, and a lease names no note.',
  'How do I get my balance back?':
    'Only the charge leaves the note. Close it and the vault pays the rest to your payout account. If the operator does not cooperate, request an exit: it can be finalized after a seven-day wait.',
};

const reduced = () =>
  typeof window !== 'undefined' && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

export function ChatDemo({
  question,
  model,
  onAsk,
}: {
  question: (typeof SUGGESTIONS)[number];
  model: string;
  onAsk: (q: (typeof SUGGESTIONS)[number]) => void;
}) {
  const answer = ANSWERS[question];
  const [step, setStep] = useState(0);
  const [typed, setTyped] = useState(0);
  const end = useRef<HTMLDivElement>(null);

  // A new question starts the play again.
  useEffect(() => {
    setStep(0);
    setTyped(0);
    const timers: number[] = [];
    const fast = reduced();
    const total = CHECK_ROWS.length;
    for (let i = 1; i <= total; i++) {
      timers.push(window.setTimeout(() => setStep(i), fast ? 0 : i * 520));
    }
    timers.push(
      window.setTimeout(
        () => {
          let shown = 0;
          const tick = () => {
            shown = Math.min(answer.length, shown + (fast ? answer.length : 2));
            setTyped(shown);
            if (shown < answer.length) timers.push(window.setTimeout(tick, 14));
          };
          tick();
        },
        fast ? 0 : total * 520 + 300,
      ),
    );
    return () => timers.forEach((t) => window.clearTimeout(t));
  }, [question, answer]);

  useEffect(() => {
    end.current?.scrollIntoView({ block: 'nearest', behavior: reduced() ? 'auto' : 'smooth' });
  }, [step, typed]);

  const checking = step < CHECK_ROWS.length || typed === 0;
  const done = typed >= answer.length;
  return (
    <div className="nc-demo" data-testid="chat-demo">
      <p className="nc-divider">
        <span>Sample answer · nothing is sent and no lease opens</span>
      </p>
      <div className="nc-turn" data-role="you">
        <span className="nc-sr">You</span>
        <p>{question}</p>
      </div>
      {checking && (
        <Seal
          title="Checking the model, then sealing the prompt"
          badge={
            <Chip tone="live">
              {step} of {CHECK_ROWS.length} passed
            </Chip>
          }
          tag="X25519 · XChaCha20-Poly1305"
          rows={CHECK_ROWS}
          done={step}
        />
      )}
      {!checking && (
        <div className="nc-turn" data-role="model">
          <span className="nc-who">{model}</span>
          <p>
            {answer.slice(0, typed)}
            {!done && <i className="nc-caret" />}
          </p>
          {done && (
            <div className="nc-meta">
              <Chip>sample answer</Chip>
              <Chip>no lease</Chip>
            </div>
          )}
        </div>
      )}
      {done && (
        <div className="nc-suggest nc-demo-more">
          {SUGGESTIONS.filter((q) => q !== question).map((q) => (
            <button key={q} type="button" onClick={() => onAsk(q)}>
              {q}
              <Icon name="right" size={14} />
            </button>
          ))}
        </div>
      )}
      <div ref={end} />
    </div>
  );
}
