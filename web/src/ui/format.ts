/** Base units to a decimal string: 1050000 with 6 decimals is "1.05". */
export function formatUnits(value: bigint | string, decimals: number): string {
  const v = BigInt(value);
  const base = 10n ** BigInt(decimals);
  const whole = v / base;
  const fraction = (v % base).toString().padStart(decimals, '0').replace(/0+$/, '');
  return `${whole}.${fraction.padEnd(2, '0')}`;
}

export function formatDate(unixSeconds: number): string {
  // `expiry` is the end of a UTC day; show the day the note is last valid.
  return new Date((unixSeconds - 1) * 1000).toISOString().slice(0, 10) + ' UTC';
}

export function download(bytes: BlobPart, fileName: string, mime: string): void {
  const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
  const link = document.createElement('a');
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  // Revoked after the browser has taken the file.
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

export const readFile = async (file: File): Promise<Uint8Array> =>
  new Uint8Array(await file.arrayBuffer());
