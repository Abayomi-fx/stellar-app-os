import { ApolloServer } from '@apollo/server';
import { HeaderMap } from '@apollo/server';
import { type NextRequest } from 'next/server';
import { NextResponse } from 'next/server';
import { resolvers } from '@/lib/graphql/resolvers';
import { typeDefs } from '@/lib/graphql/schema';
import { prisma } from '@/lib/prisma';

export const runtime = 'nodejs';

const apolloServer = new ApolloServer({
  typeDefs,
  resolvers,
  introspection: true,
});

// Apollo Server is a long-lived module singleton in the Next.js server runtime.
// Starting it once avoids a startup race when several clients hit the route.
const serverStarted = apolloServer.start();

export async function executeGraphQLRequest(request: NextRequest): Promise<Response> {
  await serverStarted;

  const httpGraphQLRequest = {
    method: request.method,
    headers: new HeaderMap(request.headers.entries()),
    search: new URL(request.url).search,
    body: request.method === 'GET' ? undefined : await request.json(),
  } as const;

  const requestBody =
    httpGraphQLRequest.body && typeof httpGraphQLRequest.body === 'object'
      ? (httpGraphQLRequest.body as { query?: string; variables?: Record<string, unknown> })
      : undefined;
  const searchParams = new URL(request.url).searchParams;
  const query = requestBody?.query ?? searchParams.get('query') ?? undefined;
  const rawVariables = searchParams.get('variables');
  const variables = requestBody?.variables ?? (rawVariables ? JSON.parse(rawVariables) : undefined);

  if (!query) {
    return NextResponse.json(
      { errors: { message: 'Must provide query string.' } },
      { status: 400 }
    );
  }

  // GDPR: User data export (DSAR)
  if (query.includes('exportUserData')) {
    const userId = variables?.userId as string;
    if (!userId) {
      return NextResponse.json(
        { errors: [ { message: 'userId is required for exportUserData.' } ] },
        { status: 400 }
      );
    }
    const exportedData = await handleExportUserData(userId);
    return NextResponse.json({ data: { exportUserData: exportedData } });
  }

  // GDPR: Right to be forgotten
  if (query.includes('deleteUserData')) {
    const userId = variables?.userId as string;
    if (!userId) {
      return NextResponse.json(
        { errors: [ { message: 'userId is required for deleteUserData.' } ] },
        { status: 400 }
      );
    }
    const deletionResult = await handleDeleteUserData(userId);
    return NextResponse.json({ data: { deleteUserData: deletionResult } });
  }

  // Extract filters from query or variables
  const region = (variables?.region as string) || extractQueryParam(query, 'region');
  const species = (variables?.species as string) || extractQueryParam(query, 'species');

  const response = await apolloServer.executeHTTPGraphQLRequest({
    httpGraphQLRequest,
    context: () => ({ request }),
  });

  const headers = new Headers();
  response.headers.forEach((value, key) => headers.set(key, value));

  if (response.body.kind !== 'complete') {
    return new Response('Streaming GraphQL responses are not supported by this route.', {
      status: 501,
      headers,
    });
  }

  return new Response(response.body.string, {
    status: response.status ?? 200,
    headers,
  });
}

async function handleExportUserData(userId: string) {
  if (!userId) throw new Error('userId is required for exportUserData.');
  const db = prisma as any;

  // Fetch the user record
  const user = await db.user.findUnique({
    where: { id: userId },
  });

  if (!user) {
    throw new Error(`User with id ${userId} not found.`);
  }

  // Fetch related personal data (adjust these model names to match your schema)
  const [sponsorships, activities] = await Promise.all([
    db.sponsorship.findMany({ where: { userId } }),
    db.activity.findMany({ where: { userId } }),
  ]);

  return {
    user,
    sponsorships,
    activities,
  };
}

async function handleDeleteUserData(userId: string) {
  if (!userId) throw new Error('userId is required for deleteUserData.');
  const db = prisma as any;

  // Delete all associated data and the user in a transaction.
  await db.$transaction([
    db.sponsorship.deleteMany({ where: { userId } }),
    db.activity.deleteMany({ where: { userId } }),
    db.user.delete({ where: { id: userId } }),
  ]);

  return {
    success: true,
    deletedUserId: userId,
  };
}

function extractQueryParam(queryStr: string, paramName: string): string | undefined {
  const regex = new RegExp(`${paramName}\s*:\s*$"[^"]+"`);
  const match = queryStr.match(regex);
  return match ? match[1] : undefined;
}

function calculateDistance(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const toRad = (x: number) => (x * Math.PI) / 180;
  const R = 6371; // km
  const dLat = toRad(lat2 - lat1);
  const dLon = toRad(lon2 - lon1);
  const a =
    Math.sin(dLat / 2) * Math.sin(dLat / 2) +
    Math.cos(toRad(lat1)) * Math.cos(toRad(lat2)) * Math.sin(dLon / 2) * Math.sin(dLon / 2);
  const c = 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
  return Math.round(R * c * 10) / 10;
}

export function POST(request: NextRequest) {
  return executeGraphQLRequest(request);
}

export function GET(request: NextRequest) {
  return executeGraphQLRequest(request);
}