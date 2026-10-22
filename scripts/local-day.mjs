// The day a timestamp falls on, in this machine's own time.
//
// The dashboard counts a day the way the person reading it does: the agents
// write their timestamps in UTC, but the day on the screen is the reader's
// own. The checks count the days the same way, or they would be comparing
// two different days.
//
//   localDay('2026-10-22T05:53:21.228Z')   // '2026-10-22' on a UTC+8 machine
//   localDay(1792648510000)                // the same, from epoch millis

export function localDay(when) {
  const d = new Date(when);
  if (Number.isNaN(d.getTime())) return '';
  // The local time, as if it were UTC.
  return new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 10);
}
