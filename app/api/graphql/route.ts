import { ApolloServer } from '@apollo/server';
import { HeaderMap } from '@apollo/server';
import { NextRequest } from 'next/server';
import { resolvers } from '@/lib/graphql/resolvers';
import { typeDefs } from '@/lib/graphql/schema';

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

  const response = await apolloServer.executeHTTPGraphQLRequest({
    httpGraphQLRequest,
    context: async () => ({ request }),
  });

  const headers = new Headers();
  response.headers.forEach((value, key) => headers.set(key, value));

  if (response.body.kind !== 'complete') {
    return new Response('Streaming GraphQL responses are not supported by this route.', {
      status: 501,
      headers,
    // Tree detail - distance from sponsor's location
    const sponsorLat = variables?.sponsorLat as number | undefined;
    const sponsorLng = variables?.sponsorLng as number | undefined;
    const treeLat = variables?.treeLat as number | undefined;
    const treeLng = variables?.treeLng as number | undefined;

    if (query.includes('treeDetail') && sponsorLat !== undefined && sponsorLng !== undefined && treeLat !== undefined && treeLng !== undefined) {
      const distance = calculateDistance(sponsorLat, sponsorLng, treeLat, treeLng);
      return NextResponse.json({
        data: {
          treeDetail: {
            distanceKm: distance,
          },
        },
      });
    }

    return NextResponse.json({
      data: {
        treeRegistryAnalytics: analyticsData,
        aggregateMetrics: analyticsData,
      },
    });
  }

  return new Response(response.body.string, {
    status: response.status ?? 200,
    headers,
  });
}

export async function POST(request: NextRequest) {
  return executeGraphQLRequest(request);
}

export async function GET(request: NextRequest) {
  return executeGraphQLRequest(request);
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
