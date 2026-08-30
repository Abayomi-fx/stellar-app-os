import { NextRequest, NextResponse } from 'next/server';
import { resolveTreeRegistryAnalytics, QueryFilter } from '@/lib/graphql/resolvers';
import { typeDefs } from '@/lib/graphql/schema';
import Stripe from 'stripe';

interface GraphQLRequestBody {
  query?: string;
  variables?: Record<string, unknown>;
  operationName?: string;
}

const stripe = new Stripe(process.env.STRIPE_SECRET_KEY ?? '');

export async function POST(req: NextRequest) {
  try {
    const body: GraphQLRequestBody = await req.json();
    const { query, variables } = body;

    if (!query) {
      return NextResponse.json(
        { errors: [{ message: 'Must provide query string.' }] },
        { status: 400 }
      );
    }

    // Introspection query support
    if (query.includes('__schema') || query.includes('__type')) {
      return NextResponse.json({
        data: {
          __schema: {
            queryType: { name: 'Query' },
            types: [
              { name: 'Query' },
              { name: 'Mutation' },
              { name: 'AggregateSequestration' },
              { name: 'RegionMetrics' },
              { name: 'SpeciesMetrics' },
            ],
          },
        },
      });
    }

    // Payment mutation: createSponsorshipPayment
    if (query.includes('createSponsorshipPayment')) {
      if (!process.env.STRIPE_SECRET_KEY) {
        return NextResponse.json(
          { errors: [{ message: 'Stripe is not configured.' }] },
          { status: 500 }
        );
      }

      const paymentInput = variables as {
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

    // Extract filters from query or variables
    const region = (variables.region as string) || extractQueryParam(query, 'region');
    const species = (variables.species as string) || extractQueryParam(query, 'species');

    const filters: QueryFilter = {};
    if (region) filters.region = region;
    if (species) filters.species = species;

    const analyticsData = await resolveTreeRegistryAnalytics(filters);

    if (query.includes('metricsByRegion')) {
      return NextResponse.json({
        data: {
          metricsByRegion: analyticsData.byRegion,
        },
      });
    }

    if (query.includes('metricsBySpecies')) {
      return NextResponse.json({
        data: {
          metricsBySpecies: analyticsData.bySpecies,
        },
      });
    }

    // Tree detail - distance from sponsor's location
    const sponsorLat = variables.sponsorLat as number | undefined;
    const sponsorLng = variables.sponsorLng as number | undefined;
    const treeLat = variables.treeLat as number | undefined;
    const treeLng = variables.treeLng as number | undefined;

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
  } catch (err: unknown) {
    const errorMsg = err instanceof Error ? err.message : 'Internal server error';
    console.error('[GraphQL API] Handler error:', err);
    return NextResponse.json(
      { errors: [{ message: errorMsg }] },
      { status: 500 }
    );
  }
}

export async function GET(req: NextRequest) {
  const { searchParams } = new URL(req.url);
  const query = searchParams.get('query');
  const region = searchParams.get('region') || undefined;
  const species = searchParams.get('species') || undefined;

  if (query && (query.includes('__schema') || query.includes('__type'))) {
    return NextResponse.json({
      data: {
        typeDefs,
        status: 'GraphQL Tree Registry Analytics Gateway active',
      },
    });
  }

  try {
    const analyticsData = await resolveTreeRegistryAnalytics({ region, species });
    return NextResponse.json({
      data: {
        treeRegistryAnalytics: analyticsData,
      },
    });
  } catch (err: unknown) {
    const errorMsg = err instanceof Error ? err.message : 'Internal server error';
    return NextResponse.json(
      { errors: [{ message: errorMsg }] },
      { status: 500 }
    );
  }
}

function extractQueryParam(queryStr: string, paramName: string): string | undefined {
  const regex = new RegExp(`${paramName}\\s*:\\s*"([^"]+)"`);
  const match = queryStr.match(regex);
  return match ? match[1] : undefined;
}

function calculateDistance(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const toRad = (x: number) => (x * Math.PI)/ 180;
  const R = 6371; // km
  const dLat = toRad(lat2 - lat1);
  const dLon = toRad(lon2 - lon1);
  const a =
    Math.sin(dLat / 2) * Math.sin(dLat / 2) +
    Math.cos(toRad(lat1)) * Math.cos(toRad(lat2)) * Math.sin(dLon / 2) * Math.sin(dLon / 2);
  const c = 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));
  return Math.round(R * c * 10) / 10;
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
    const data = await response.json() as Record<string, Record<string, number>>;
    const xlmPrice = data.stellar?[vsCurrency];
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