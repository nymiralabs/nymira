import { useId, useMemo, useState } from 'react';
import { charge, costNano } from '../chat/charge';
import { chargeParams } from '../chat/session';
import { MODELS } from '../chat/models';
import { PROFILE } from '../profile';

// The lease arithmetic, with the app's own functions (`chat/charge.ts`, which follows the
// operator's `charge.rs`) and the vault parameters pinned in this build.
const MICRO = 1_000_000;
const usd = (micro: bigint | number, places = 3) => (Number(micro) / MICRO).toFixed(places);

export function ChargeCalc() {
  const id = useId();
  const params = chargeParams(PROFILE);
  const classes = (PROFILE.pins?.config.bound_classes ?? ['100000', '500000']).map((c) =>
    BigInt(c),
  );
  const [model, setModel] = useState(MODELS[0]!.id);
  const [prompt, setPrompt] = useState(1200);
  const [answer, setAnswer] = useState(300);
  const [held, setHeld] = useState(90);
  const [bound, setBound] = useState(classes[0]!);
  const out = useMemo(() => {
    const spec = MODELS.find((m) => m.id === model) ?? MODELS[0]!;
    const cost = costNano(spec, BigInt(prompt), BigInt(answer));
    const usage = Number(cost) / 1000;
    const withMargin = (usage * 1.03).toFixed(2);
    const slot = (Number(bound) * Number(params.slotRate) * held) / 60_000_000;
    const total = charge(bound, cost, BigInt(held), params);
    return { cost, usage, withMargin, slot, total };
  }, [model, prompt, answer, held, bound, params]);
  const cellsTotal = 50;
  const filled = Math.max(1, Math.ceil(Number(out.total) / (Number(bound) / cellsTotal)));
  return (
    <div className="nd-calc" data-testid="docs-calc">
      <div className="nd-calc-form">
        <label>
          <span>Model</span>
          <select value={model} onChange={(e) => setModel(e.target.value)}>
            {MODELS.map((m) => (
              <option key={m.id} value={m.id}>
                {m.id}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Prompt tokens</span>
          <input
            type="range"
            min={100}
            max={120000}
            step={100}
            value={prompt}
            aria-describedby={`${id}-p`}
            onChange={(e) => setPrompt(Number(e.target.value))}
          />
          <output id={`${id}-p`}>{prompt.toLocaleString('en-US')}</output>
        </label>
        <label>
          <span>Answer tokens</span>
          <input
            type="range"
            min={10}
            max={8000}
            step={10}
            value={answer}
            onChange={(e) => setAnswer(Number(e.target.value))}
          />
          <output>{answer.toLocaleString('en-US')}</output>
        </label>
        <label>
          <span>Seconds the lease is held</span>
          <input
            type="range"
            min={10}
            max={600}
            step={10}
            value={held}
            onChange={(e) => setHeld(Number(e.target.value))}
          />
          <output>{held} s</output>
        </label>
        <fieldset>
          <legend>Lease class</legend>
          {classes.map((c) => (
            <label key={c.toString()} className="nd-calc-radio">
              <input
                type="radio"
                name={`${id}-bound`}
                checked={bound === c}
                onChange={() => setBound(c)}
              />
              <span>{usd(c, 2)} USDC</span>
            </label>
          ))}
        </fieldset>
      </div>
      <div className="nd-calc-out">
        <dl>
          <div>
            <dt>Usage at the pinned prices</dt>
            <dd>{out.usage.toFixed(1)} micro-USD</dd>
          </div>
          <div>
            <dt>Plus 3%</dt>
            <dd>{out.withMargin} micro-USD</dd>
          </div>
          <div>
            <dt>Slot fee, {Number(params.slotRate) / 10_000}% of the bound per minute held</dt>
            <dd>{out.slot.toFixed(1)} micro-USD</dd>
          </div>
          <div>
            <dt>
              Rounded up to a step of {usd(params.step)}, at least {usd(params.minCharge)}, at most
              the bound
            </dt>
            <dd data-testid="docs-calc-charge">
              <strong>{usd(out.total)} USDC</strong>
            </dd>
          </div>
        </dl>
        <div
          className="nd-calc-cells"
          role="img"
          aria-label={`The charge, ${usd(out.total)} USDC, against the ${usd(bound, 2)} USDC bound`}
        >
          {Array.from({ length: cellsTotal }, (_, i) => (
            <i key={i} data-on={i < filled} />
          ))}
        </div>
        <p className="nd-calc-cap">
          Each cell is {usd(Number(bound) / cellsTotal, 4)} USDC. The strip is the bound; the filled
          cells are what the lease charges. The rest goes back to the note.
        </p>
      </div>
    </div>
  );
}
