'use client';

import React from 'react';
import Link from 'next/link';
import { Megaphone, ArrowRight } from 'lucide-react';
import { useReferralStats } from '@/hooks/useReferralStats';
import ReferralLinkCard from '@/components/ReferralLinkCard';
import StatsDisplay from '@/components/StatsDisplay';
import RewardTiers from '@/components/RewardTiers';
import SocialShareButtons from '@/components/SocialShareButtons';

export default function ReferralProgramPage() {
  const { stats, loading, error } = useReferralStats();

  if (loading) {
    return (
      <main className="flex items-center justify-center min-h-screen">
        <p>Loading…</p>
      </main>
    );
  }

  if (error) {
    return (
      <main className="flex items-center justify-center min-h-screen">
        <p role="alert" className="text-red-600">
          Error: {error}
        </p>
      </main>
    );
  }

  if (!stats) return null;

  return (
    <main className="mx-auto max-w-3xl px-4 py-8">
      <Link
        href="/affiliate"
        className="mb-6 flex items-center justify-between gap-3 rounded-lg border border-stellar-blue/20 bg-stellar-blue/5 p-4 transition-colors hover:bg-stellar-blue/10"
      >
        <div className="flex items-center gap-3">
          <Megaphone className="h-5 w-5 text-stellar-blue" aria-hidden />
          <div>
            <div className="text-sm font-semibold">Looking for the Affiliate program?</div>
            <div className="text-sm text-muted-foreground">
              Partners and influencers earn 10–25% on referred sponsors.
            </div>
          </div>
        </div>
        <ArrowRight className="h-5 w-5 text-stellar-blue" aria-hidden />
      </Link>

      <h1 className="text-3xl font-bold mb-2">Referral Program</h1>
      <p className="text-gray-500 mb-6">
        Share your unique link with friends and earn rewards for every signup.
      </p>

      <ReferralLinkCard referralLink={stats.referralLink} />

      <StatsDisplay referralsCount={stats.referralsCount} totalEarnings={stats.totalEarnings} />

      <RewardTiers tiers={stats.tiers} />

      <SocialShareButtons url={stats.referralLink} title="Join me on Stellar Farm Credit" />
    </main>
  );
}
