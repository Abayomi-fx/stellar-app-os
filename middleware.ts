import { NextRequest, NextResponse } from 'next/server';
import { MemoryRateLimiter } from '@/lib/rate-limit';
export const runtime = 'edge';
const limiter = new MemoryRateLimiter();
export async function middleware(req: NextRequest) {
  const { pathname } = req.nextUrl;
  if (!pathname.startsWith('/api/')) return NextResponse.next();
  const ip = req.headers.get('x-forwarded-for')?.split(',')[0] || 'unknown';
  const auth = req.headers.get('authorization');
  const apiKey = req.headers.get('x-api-key');
  const key = auth ? `user:$${auth}` : apiKey ? `apikey:$apiKey`: `ip:$ip`;
  const res = await limiter.limit(key, { windowMs: 60000, maxRequests: 100 });
  if (!res.success) {
    return NextResponse.json({ error: 'Too Many Requests' }, { status: 429, headers: { 'Retry-After': String(res.retryAfter ?? 60) } });
  }
  const response = NextResponse.next();
  response.headers.set('X-RateLimit-Remaining', String(res.remaining));
  return response;
}
export const config = { matcher: '/api/:path*' };