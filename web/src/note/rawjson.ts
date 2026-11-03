// The zkapi wallet core writes u64 values (the chain id) as bare digits above 2^53. JSON.parse
// would round them, and a rounded state or request no longer verifies. These helpers cut a JSON
// object into the exact text of its members without parsing the numbers.

/** The exact JSON text of each top-level member of the object `json`. */
export function splitObject(json: string): Record<string, string> {
  const out: Record<string, string> = {};
  let i = 0;
  const skipSpace = () => {
    while (i < json.length && ' \n\r\t'.includes(json[i]!)) i++;
  };
  const readString = (): string => {
    const start = i;
    i++; // opening quote
    while (i < json.length && json[i] !== '"') i += json[i] === '\\' ? 2 : 1;
    i++; // closing quote
    return json.slice(start, i);
  };
  const readValue = (): string => {
    const start = i;
    if (json[i] === '"') {
      readString();
    } else if (json[i] === '{' || json[i] === '[') {
      let depth = 0;
      while (i < json.length) {
        const c = json[i]!;
        if (c === '"') {
          readString();
          continue;
        }
        if (c === '{' || c === '[') depth++;
        if (c === '}' || c === ']') depth--;
        i++;
        if (depth === 0) break;
      }
    } else {
      while (i < json.length && !',}] \n\r\t'.includes(json[i]!)) i++;
    }
    return json.slice(start, i);
  };
  skipSpace();
  if (json[i] !== '{') throw new Error('not a JSON object');
  i++;
  skipSpace();
  while (json[i] !== '}') {
    if (json[i] !== '"') throw new Error('malformed JSON object');
    const key = JSON.parse(readString()) as string;
    skipSpace();
    if (json[i] !== ':') throw new Error('malformed JSON object');
    i++;
    skipSpace();
    out[key] = readValue();
    skipSpace();
    if (json[i] === ',') {
      i++;
      skipSpace();
    }
    if (i >= json.length) throw new Error('malformed JSON object');
  }
  return out;
}
