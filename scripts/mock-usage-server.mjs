// A stand-in for a provider's usage endpoint, so the custom-provider path
// can be checked against something that answers.
//
//   node scripts/mock-usage-server.mjs [port]

import { createServer } from 'node:http';

const port = Number(process.argv[2] || 8799);
const body = JSON.stringify({
  data: {
    usage: {
      percent: 42.5,
      reset_at: '2026-10-26T18:59:59Z',
      label: 'Monthly allowance',
    },
  },
});

createServer((req, res) => {
  const auth = req.headers.authorization;
  if (auth !== 'Bearer sk-the-token') {
    res.writeHead(401, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: { message: 'Missing Authentication header', code: 401 } }));
    return;
  }
  res.writeHead(200, { 'Content-Type': 'application/json' });
  res.end(body);
}).listen(port, '127.0.0.1', () => {
  console.log(`listening on http://127.0.0.1:${port}`);
});
