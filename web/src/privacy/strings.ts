// The privacy sentences `nymirad status` also prints. They live in `shared/privacy-strings.json`
// at the repository root; `nymirad` compiles in the same file, and a test pins its SHA-256 on
// both sides. Sentences only this panel says are in `text.ts`.
import raw from '../../../shared/privacy-strings.json';

const table = raw as Record<string, string>;

export type StringKey = keyof typeof raw;

/** The sentence for `key`. */
export function sentence(key: StringKey): string {
  const text = table[key];
  if (text === undefined) throw new Error(`shared/privacy-strings.json has no key ${key}`);
  return text;
}

/** The sentence with each `{name}` replaced. */
export function fill(key: StringKey, vars: Record<string, string | number>): string {
  let text = sentence(key);
  for (const [name, value] of Object.entries(vars)) {
    text = text.replaceAll(`{${name}}`, String(value));
  }
  return text;
}
