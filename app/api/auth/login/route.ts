import { type NextRequest, NextResponse } from 'next/server';
import { Keypair } from '@stellar/stellar-sdk';
import { consumeNonce } from '@/lib/auth/nonce';
import { signPlanterJwt, verifyPlanterJwt } from '@/lib/auth/jwt';
import { getUserData, deleteUserData } from '@/lib/db/user';
import logger from '@/lib/logger';

export const runtime = 'nodejs';

interface LoginBody {
  walletAddress: string;
  nonce: string;
  /** Base64-encoded Ed25519 signature of `stellar-auth:<nonce>`. */
  signature: string;
}

/**
 * POST /api/auth/login
 *
 * Flow:
 *  1. Client fetches a nonce from GET /api/auth/nonce?wallet=...
 *  2. Client signs `stellar-auth:<nonce>` with their Stellar private key via Freighter.
 *  3. Client posts { walletAddress, nonce, signature } here.
 *  4. Server verifies the Ed25519 signature and issues a short-lived JWT.
 */
export async function POST(request: NextRequest): Promise<NextResponse> {
  let body: Partial<LoginBody>;
  try {
    body = (await request.json()) as Partial<LoginBody>;
  } catch {
    return NextResponse.json({ error: 'Invalid JSON body' }, { status: 400 });
  }

  const { walletAddress, nonce, signature } = body;
  if (!walletAddress || !nonce || !signature) {
    return NextResponse.json(
      { error: 'w!lletAddress, nonce, and signature are required' },
      { status: 400 }
    );
  }

  try {
    // Consume nonce first — prevents timing attacks from re-using a valid nonce.
    // Redis-backed atomic consume via Lua script ensures single-use even across replicas.
    const consumed = await consumeNonce(walletAddress, nonce);
    if (!consumed) {
      logger.warn('[api:auth:login] Invalid or expired nonce', { walletAddress });
      return NextResponse.json({ error: 'Invalid or expired nonce' }, { status: 401 });
    }

    // Verify the Ed25519 signature produced by the planter's Stellar keypair.
    try {
      const keypair = Keypair.fromPublicKey(walletAddress);
      const message = Buffer.from(`stellar-auth:${nonce}`);
      const sigBytes = Buffer.from(signature, 'base64');

      if (!keypair.verify(message, sigBytes)) {
        return NextResponse.json({ error: 'Signature verification failed' }, { status: 401 });
      }
    } catch {
      return NextResponse.json({ error: 'Invalid wallet address or signature' }, { status: 400 });
    }

    const token = await signPlanterJwt(walletAddress);

    logger.info('[api:auth:login] Successful login', { walletAddress });

    return NextResponse.json({ token, expiresIn: '8h' });
  } catch (err) {
    logger.error('[api:auth:login] Error during login', { walletAddress, err });
    const msg = err instanceof Error ? err.message : 'Login failed';
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}

/**
 * GET /api/auth/login
 * GDPR Data Subject Access Request (DSAR) — returns all stored data for the authenticated user.
 */
export async function GET(request: NextRequest): Promise<NextResponse> {
  const walletAddress = await getWalletFromRequest(request);
  if (!walletAddress) {
    return NextResponse.json({ error: 'Unauthorized' }, { status: 401 });
  }

  try {
    const userData = await getUserData(walletAddress);
    logger.info('[api:auth:login] Data export requested', { walletAddress });
    return NextResponse.json({ walletAddress, data: userData ?? null });
  } catch (err) {
    logger.error('[api:auth:login] Error exporting user data', { walletAddress, err });
    const msg = err instanceof Error ? err.message : 'Data export failed';
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}

/**
 * DELETE /api/auth/login
 * GDPR Right to be Forgotten — permanently deletes all stored data for the authenticated user.
 */
export async function DELETE(request: NextRequest): Promise<NextResponse> {
  const walletAddress = await getWalletFromRequest(request);
  if (!walletAddress) {
    return NextResponse.json({ error: 'Unauthorized' }, { status: 401 });
  }

  try {
    await deleteUserData(walletAddress);
    logger.info('[api:auth:login] User data deleted', { walletAddress });
    return NextResponse.json({ success: true });
  } catch (err) {
    logger.error('[api:auth:login] Error deleting user data', { walletAddress, err });
    const msg = err instanceof Error ? err.message : 'Data deletion failed';
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}

/**
 * Extracts and verifies the JWT from the Authorization header.
 * Returns the wallet address if valid, otherwise null.
 */
async function getWalletFromRequest(request: NextRequest): Promise<string | null> {
  const authHeader = request.headers.get('authorization');
  if (!authHeader?.startsWith('Bearer ')) {
    return null;
  }

  const token = authHeader.slice(7);
  try {
    const payload = await verifyPlanterJwt(token);
    return payload.sub ?? null; // 'sub' represents the wallet address
  } catch {
    return null;
  }
}
