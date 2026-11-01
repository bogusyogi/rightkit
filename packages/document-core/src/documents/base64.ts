/** Encode typed bytes without spreading a large Uint8Array onto the call stack. */
export function bytesToBase64(bytes: Uint8Array, chunkSize = 0x8000): string {
  const chunks: string[] = [];
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    const end = Math.min(bytes.length, offset + chunkSize);
    let binary = "";
    for (let index = offset; index < end; index += 1) {
      binary += String.fromCharCode(bytes[index] ?? 0);
    }
    chunks.push(binary);
  }
  return btoa(chunks.join(""));
}
