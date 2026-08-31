import { proxy } from './proxy';
import { handleCorsPreflight, getCorsHeaders } from './lib/cors';
import { type NextRequest } from 'next/server';

export async function middleware(request: NextRequest) {
  const preflight = handleCorsPreflight(request);
  if (preflight) return preflight;
  const response = await proxy(request);
  const cors = getCorsHeaders(request.headers.get('origin'));
  for (const [k, v] of Object.entries(cors)) response.headers.set(k, v);
  return response;
}

export const config = { matcher: ['/api/:path*'] };
