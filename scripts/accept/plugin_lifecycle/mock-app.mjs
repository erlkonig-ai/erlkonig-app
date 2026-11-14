// Harmless stand-in for the same executable's mcp-stdio entry point.
import { createInterface } from 'node:readline';

if (process.argv[2] !== 'mcp-stdio') process.exit(2);
const input = createInterface({ input: process.stdin });
input.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  let result;
  if (message.method === 'initialize') {
    result = {
      protocolVersion: '2025-03-26', capabilities: { tools: {} },
      serverInfo: { name: 'erlkonig-harmless-proof', version: '0.0.1' },
    };
  } else if (message.method === 'tools/list') {
    result = { tools: [{
      name: 'probe', description: 'Return a harmless fixed response.',
      inputSchema: { type: 'object', properties: {} },
      annotations: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
    }] };
  } else if (message.method === 'tools/call') {
    result = { content: [{ type: 'text', text: 'harmless-proof' }] };
  } else {
    result = {};
  }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }) + '\n');
});
