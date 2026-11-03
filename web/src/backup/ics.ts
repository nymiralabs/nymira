// The expiry reminder. A calendar file with a generic title: no note ID, commitment, amount, vault,
// payout account or product name. It is an all-day event on the UTC day the note expires (the vault rounds the
// expiry to the end of a UTC day), with alarms 5 and 3 days before, the days the app offers to
// close and to start an exit. A calendar that syncs to a provider tells that provider the date.

const pad = (n: number) => String(n).padStart(2, '0');

function dateValue(unixSeconds: number): string {
  // `expiry` is the end of a UTC day, so the last second of that day is expiry - 1.
  const d = new Date((unixSeconds - 1) * 1000);
  return `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}`;
}

export function expiryCalendar(expiryUnixSeconds: number, uid: string, nowMs: number): string {
  const stamp = new Date(nowMs)
    .toISOString()
    .replace(/[-:]/g, '')
    .replace(/\.\d{3}Z$/, 'Z');
  const alarm = (days: number) =>
    [
      'BEGIN:VALARM',
      'ACTION:DISPLAY',
      'DESCRIPTION:Prepaid note reminder',
      `TRIGGER:-P${days}D`,
      'END:VALARM',
    ].join('\r\n');
  return [
    'BEGIN:VCALENDAR',
    'VERSION:2.0',
    'PRODID:-//Reminder//EN',
    'BEGIN:VEVENT',
    `UID:${uid}@reminder.invalid`,
    `DTSTAMP:${stamp}`,
    `DTSTART;VALUE=DATE:${dateValue(expiryUnixSeconds)}`,
    'SUMMARY:Prepaid note expires today',
    'DESCRIPTION:Close the prepaid note you funded before it expires.',
    alarm(5),
    alarm(3),
    'END:VEVENT',
    'END:VCALENDAR',
    '',
  ].join('\r\n');
}
