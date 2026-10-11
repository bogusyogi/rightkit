import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import type { Duplex } from 'node:stream';

/** Tiny RFC6455 fixture server; production transport uses built-in WebSocket. */
export async function mockCdp(
  handler: (message: any, send: (message: any) => void) => void,
  options: { http?: (path: string) => unknown } = {},
) {
  const sockets = new Set<Duplex>();
  const messages: any[] = [];
  const httpRequests: string[] = [];
  const senders = new Set<(message: any) => void>();
  // Plain HTTP for /json/version & /json discovery on the same port; unknown paths are 404.
  const server = createServer((request, response) => {
    httpRequests.push(request.url ?? '');
    const body = options.http?.(request.url ?? '');
    response.writeHead(body === undefined ? 404 : 200, { 'Content-Type': 'application/json' });
    response.end(body === undefined ? '' : JSON.stringify(body));
  });
  server.on('upgrade', (request, socket) => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket));
    const accept = createHash('sha1').update(request.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const send = (message: any) => {
      const payload = Buffer.from(JSON.stringify(message));
      const header = Buffer.alloc(payload.length < 126 ? 2 : 4);
      header[0] = 0x81;
      if (payload.length < 126) header[1] = payload.length;
      else { header[1] = 126; header.writeUInt16BE(payload.length, 2); }
      socket.write(Buffer.concat([header, payload]));
    };
    senders.add(send); socket.on('close', () => senders.delete(send));
    let buffer = Buffer.alloc(0);
    socket.on('data', (data: Buffer) => {
      buffer = Buffer.concat([buffer, data]);
      while (buffer.length >= 2) {
        const opcode = buffer[0] & 15, masked = !!(buffer[1] & 128);
        let length = buffer[1] & 127, offset = 2;
        if (length === 126) { if (buffer.length < 4) return; length = buffer.readUInt16BE(2); offset = 4; }
        if (length === 127) { if (buffer.length < 10) return; length = Number(buffer.readBigUInt64BE(2)); offset = 10; }
        const start = offset + (masked ? 4 : 0);
        if (buffer.length < start + length) return;
        const payload = Buffer.from(buffer.subarray(start, start + length));
        if (masked) for (let i = 0; i < length; i++) payload[i] ^= buffer[offset + i % 4];
        buffer = buffer.subarray(start + length);
        if (opcode === 8) { socket.end(Buffer.from([0x88, 0])); return; }
        if (opcode === 1) { const message = JSON.parse(payload.toString()); messages.push(message); handler(message, send); }
      }
    });
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  return {
    endpoint: `ws://127.0.0.1:${(server.address() as { port: number }).port}`,
    messages,
    httpRequests,
    emit: (message: any) => { for (const send of senders) send(message); },
    disconnect: () => { for (const socket of sockets) socket.destroy(); },
    close: async () => { for (const socket of sockets) socket.destroy(); await new Promise<void>((resolve) => server.close(() => resolve())); },
  };
}

export function pageResponder(message: any, send: (message: any) => void) {
  const result = message.method === 'Target.createTarget' ? { targetId: 'target' }
    : message.method === 'Target.attachToTarget' ? { sessionId: 'session' }
    : message.method === 'Page.getFrameTree' ? { frameTree: { frame: { id: 'main' } } }
    : message.method.startsWith('Runtime.call') || message.method === 'Runtime.evaluate' ? { result: { value: true } } : {};
  if (message.method === 'Runtime.enable') {
    for (const [id, frameId, isDefault] of [[11, 'main', false], [12, 'child', true], [13, 'main', true]]) {
      send({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id, auxData: { frameId, isDefault } } } });
    }
  }
  send({ id: message.id, result });
}
