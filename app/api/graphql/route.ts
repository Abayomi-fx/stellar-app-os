import { ApolloServer } from '@apollo/server';
import { HeaderMap } from '@apollo/server';
import { type NextRequest } from 'next/server';
import { NextResponse } from 'next/server';
import { resolvers } from '@/lib/graphql/resolvers';
import { typeDefs } from '@/lib/graphql/schema';
import Stripe from 'stripe';

export const runtime = 'nodejs';

const stripe = new Stripe(process.env.STRIPE_SECRET_KEY ?? '');

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

  const body = request.method === 'GET' ? undefined : await request.json();

  // Payment mutation: createSponsorshipPayment
  if (request.method === 'POST' && body && typeof body === 'object' && 'query' in body) {
    const { query, variables } = body as {
      query?: string;
      variables?: Record<string, unknown>;
    };

    if (query?.includes('createSponsorshipPayment')) {
      if (!process.env.STRIPE_SECRET_KEY) {
        return NextResponse.json(
          { errors: [{ message: 'Stripe is not configured.' }] },
          { status: 500 }
        );
      }

      const paymentInput = (variables ?? {}) as {
        amount?: number;
        currency?: string;
        paymentMethodId?: string;
        description?: string;
      };

      const { amount, currency, paymentMethodId, description } = paymentInput;

      if (!amount || !currency || !paymentMethodId) {
        return NextResponse.json(
          { errors: [{ message: 'Missing required parameters: amount, currency, paymentMethodId' }] },
          { status: 400 }
        );
      }

      try {
        const paymentIntent = await stripe.paymentIntents.create({
          amount: Math.round(amount * 100), // Stripe expects minor units
          currency: currency.toLowerCase(),
          payment_method: paymentMethodId,
          confirm: true,
          automatic_payment_methods: { enabled: true, allow_redirects: 'never' },
          description: description || 'Tree sponsorship',
        });

        // Convert the fiat amount to XLM automatically
        const xlmAmount = await convertToXlm(amount, currency);

        // Settle the contract on XLM (placeholder)
        await settleXlmContract(xlmAmount);

        return NextResponse.json({
          data: {
            createSponsorshipPayment: {
              success: true,
              paymentIntentId: paymentIntent.id,
              amount: xlmAmount,
              currency: 'XLM',
              status: 'succeeded',
            },
          },
        });
      } catch (error) {
        const errorMsg = error instanceof Error ? error.message : 'Payment processing failed';
        console.error('[GraphQL API] Payment error:', error);
        return NextResponse.json(
          { errors: [{ message: errorMsg }] },
          { status: 400 }
        );
      }
    }
  }

  const httpGraphQLRequest = {
    method: request.method,
    headers: new HeaderMap(request.headers.entries()),
    search: new URL(request.url).search,
    body,
  } as const;

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

export function POST(request: NextRequest) {
  return executeGraphQLRequest(request);
}

export function GET(request: NextRequest) {
  return executeGraphQLRequest(request);
}

async function convertToXlm(amount: number, currency: string): Promise<number> {
  const vsCurrency = currency.toLowerCase();
  try {
    const response = await fetch(
      `https://api.coingecko.com/api/v3/simple/price?ids=stellar&vs_currencies=${vsCurrency}`
    );
    if (!response.ok) {
      throw new Error('Failed to fetch XLM exchange rate');
    }
    const data = (await response.json()) as Record<string, Record<string, number>>;
    const xlmPrice = data.stellar?.[vsCurrency];
    if (!xlmPrice || xlmPrice <= 0) {
      throw new Error(`Could not retrieve XLM price in ${currency}`);
    }
    return amount / xlmPrice;
  } catch (error) {
    console.error('FX conversion error:', error);
    throw new Error('Currency conversion to XLM failed');
  }
}

async function settleXlmContract(xlmAmount: number): Promise<void> {
  // TODO: Integrate with Stellar smart contract to record sponsorship
  console.log(`Settling sponsorship contract with ${xlmAmount.toFixed(7)} XLM`);
}