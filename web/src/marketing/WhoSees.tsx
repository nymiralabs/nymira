import { sentence, type StringKey } from '../privacy/strings';

// Who learns what, in the sentences `nymirad status` and the app's privacy panel also use
// (shared/privacy-strings.json), so the landing cannot drift from them.
const PARTIES: readonly [party: string, key: StringKey][] = [
  ['Operator', 'who_operator'],
  ['Model', 'who_model'],
  ['NEAR AI', 'who_near_ai'],
  ['NEAR chain', 'who_chain'],
  ['RPC providers', 'who_rpc'],
  ['Web host', 'who_host'],
];

export function WhoSees() {
  return (
    <div className="nm-who" data-testid="who-sees-what">
      <p className="nm-who-lead" data-testid="who-desc">
        You hold the note secret and your prompts. {sentence('who_operator')}{' '}
        {sentence('who_model')} {sentence('who_chain')}
      </p>
      <dl>
        {PARTIES.map(([party, key]) => (
          <div key={key}>
            <dt>{party}</dt>
            <dd>{sentence(key)}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
