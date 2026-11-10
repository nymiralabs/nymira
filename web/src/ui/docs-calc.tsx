import { useId, useMemo, useState } from 'react';
import { charge, costNano } from '../chat/charge';
import { chargeParams } from '../chat/session';
import { MODELS } from '../chat/models';
import { PROFILE } from '../profile';
import { vars } from '../marketing/kit';

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
  const back = Number(bound) - Number(out.total);
  const parts = [out.usage * 1.03, out.slot];
  const top = Math.max(...parts, 1);
  const range = (min: number, max: number, v: number) =>
    vars({ '--p': `${((v - min) / (max - min)) * 100}%` });
  const preset = (p: number, a: number, h: number) => {
    setPrompt(p);
    setAnswer(a);
    setHeld(h);
  };
  const PRESETS: [string, number, number, number][] = [
    ['Short chat', 1200, 300, 90],
    ['Long document', 60000, 2000, 300],
    ['Longest job', 120000, 8000, 600],
  ];
  return (
    <div className="nd-calc" data-testid="docs-calc">
      <div className="nd-calc-presets">
        <span>Start from</span>
        {PRESETS.map(([name, p, a, h]) => (
          <button
            key={name}
            type="button"
            aria-pressed={prompt === p && answer === a && held === h}
            onClick={() => preset(p, a, h)}
          >
            {name}
          </button>
        ))}
      </div>
      <div className="nd-calc-body">
        <div className="nd-calc-form">
          <label className="nd-calc-field">
            <span>Model</span>
            <select value={model} onChange={(e) => setModel(e.target.value)}>
              {MODELS.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.id}
                </option>
              ))}
            </select>
          </label>
          <label className="nd-calc-field">
            <span>
              Prompt tokens <output id={`${id}-p`}>{prompt.toLocaleString('en-US')}</output>
            </span>
            <input
              type="range"
              min={100}
              max={120000}
              step={100}
              value={prompt}
              style={range(100, 120000, prompt)}
              aria-describedby={`${id}-p`}
              onChange={(e) => setPrompt(Number(e.target.value))}
            />
          </label>
          <label className="nd-calc-field">
            <span>
              Answer tokens <output>{answer.toLocaleString('en-US')}</output>
            </span>
            <input
              type="range"
              min={10}
              max={8000}
              step={10}
              value={answer}
              style={range(10, 8000, answer)}
              onChange={(e) => setAnswer(Number(e.target.value))}
            />
          </label>
          <label className="nd-calc-field">
            <span>
              Seconds the lease is held <output>{held} s</output>
            </span>
            <input
              type="range"
              min={10}
              max={600}
              step={10}
              value={held}
              style={range(10, 600, held)}
              onChange={(e) => setHeld(Number(e.target.value))}
            />
          </label>
          <fieldset className="nd-calc-field">
            <legend>Lease class (the bound)</legend>
            <div className="nd-calc-seg" role="group" aria-label="Lease class">
              {classes.map((c) => (
                <button
                  key={c.toString()}
                  type="button"
                  aria-pressed={bound === c}
                  onClick={() => setBound(c)}
                >
                  {usd(c, 2)} USDC
                </button>
              ))}
            </div>
          </fieldset>
        </div>
        <div className="nd-calc-out">
          <div className="nd-calc-total">
            <small>The lease charges</small>
            <strong data-testid="docs-calc-charge">{usd(out.total)} USDC</strong>
            <span>
              {usd(back, 3)} USDC of the {usd(bound, 2)} USDC bound goes back to the note.
            </span>
          </div>
          <div>
            <div
              className="nd-calc-cells"
              role="img"
              aria-label={`The charge, ${usd(out.total)} USDC, against the ${usd(bound, 2)} USDC bound`}
            >
              {Array.from({ length: cellsTotal }, (_, i) => (
                <i key={i} data-on={i < filled} style={vars({ '--n': i })} />
              ))}
            </div>
          </div>
          <dl>
            <div>
              <dt>Usage at the pinned prices</dt>
              <dd>{out.usage.toFixed(1)} micro-USD</dd>
            </div>
            <div>
              <dt>Plus 3%</dt>
              <dd>{out.withMargin} micro-USD</dd>
              <span className="nd-calc-bar" aria-hidden="true">
                <i style={vars({ '--w': (parts[0]! / top) * 100 })} />
              </span>
            </div>
            <div>
              <dt>Slot fee, {Number(params.slotRate) / 10_000}% of the bound per minute held</dt>
              <dd>{out.slot.toFixed(1)} micro-USD</dd>
              <span className="nd-calc-bar" aria-hidden="true">
                <i style={vars({ '--w': (parts[1]! / top) * 100 })} />
              </span>
            </div>
            <div>
              <dt>
                Rounded up to a step of {usd(params.step)}, at least {usd(params.minCharge)}, at
                most the bound
              </dt>
              <dd>{usd(out.total)} USDC</dd>
            </div>
          </dl>
          <p className="nd-calc-cap">
            Each cell is {usd(Number(bound) / cellsTotal, 4)} USDC. The strip is the bound; the
            filled cells are what the lease charges.
          </p>
        </div>
      </div>
    </div>
  );
}
