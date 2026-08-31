import { proxy } from './proxy';

export { proxy as middleware };

export const config = {
  matcher: ['/api/:path*'],
};
