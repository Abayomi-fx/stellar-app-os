import { type NextRequest, NextResponse } from 'next/server';
import { isValidStellarAddress, listBySponsor } from '@/lib/api/carbon-impact';

export const runtime = 'nodejs';

interface Tree {
  species?: string;
  co2Offset?: number;
}

interface SponsorImpact {
  totalCo2Offset: number;
  treeCount: number;
  perSpecies: Record<string, number>;
  cachedAt: string;
}

export async function GET(
  _request: NextRequest,
  { params }: { params: Promise<{ sponsor: string }> }
) {
  try {
    const { sponsor: rawSponsor } = await params;
    const sponsor = rawSponsor?.trim() ?? '';

    if (!sponsor) {
      return NextResponse.json({ error: 'sponsor address is required' }, { status: 400 });
    }

    if (!isValidStellarAddress(sponsor)) {
      return NextResponse.json(
        { error: 'Invalid Stellar address &mdash; must be a 56-character G&ielips3 public key' },
        { status: 400 }
      );
    }

    // Aggregate all trees for the sponsor, handling pagination for large datasets.
    let cursor: string | null = null;
    const perSpecies: Record<string, number> = {};
    let totalCo2Offset = 0;
    let treeCount = 0;

    do {
      const page = await listBySponsor(sponsor, cursor);
      for (const tree of page.trees ?? []) {
        treeCount += 1;
        totalCo2Offset += tree.co2Offset ?? 0;
        const species = tree.species ?? 'Unknown';
        perSpecies[species] = (perSpecies[species] ?? 0) + 1;
      }
      cursor = page.nextCursor ?? null;
    } while (cursor);

    const impact: SponsorImpact = {
      totalCo2Offset,
      treeCount,
      perSpecies,
      cachedAt: new Date().toISOString(),
    };

    return NextResponse.json(impact, {
      headers: {
        'Cache-Control': 'public, s-maxage=30, stale-while-revalidate=10',
        'X-Cached-At': impact.cachedAt,
      },
    });
  } catch (err) {
    console.error('[api/impact/:sponsor] error:', err);
    return NextResponse.json(
      { error: err instanceof Error ? err.message : 'Internal server error' },
      { status: 500 }
    );
  }
}
