/**
 * Species Voting — On-chain governance for adding new tree species
 *
 * Token holders can propose and vote for new species to be added to the
 * species catalogue. Voting power is proportional to TREE token holdings.
 */

import type { NetworkType } from '@/lib/types/wallet';

// ── Types ─────────────────────────────────────────────────────────────────────

export enum ProposalStatus {
  Active = 'active',
  Passed = 'passed',
  Rejected = 'rejected',
  Executed = 'executed',
}

export interface ProposalRecord {
  id: number;
  slug: string;
  name: string;
  co2_scaled: number;
  maturity_years: number;
  proposer: string;
  votes_for: number;
  votes_against: number;
  status: ProposalStatus;
  created_at: number;
  voting_ends_at: number;
}

export interface VoteRecord {
  voter: string;
  vote_for: boolean;
  power: number;
  voted_at: number;
}

// ── Contract configuration ─────────────────────────────────────────────────────

// TODO: Update with actual deployed contract addresses
export const SPECIES_VOTING_CONTRACT_TESTNET = '' as const;
export const SPECIES_VOTING_CONTRACT_MAINNET = '' as const;

export function getSpeciesVotingContract(network: NetworkType): string {
  const address =
    network === 'mainnet' ? SPECIES_VOTING_CONTRACT_MAINNET : SPECIES_VOTING_CONTRACT_TESTNET;
  if (!address) {
    throw new Error('Species voting contract not deployed for this network');
  }
  return address;
}

// ── Transaction builders ───────────────────────────────────────────────────────

/**
 * Build a transaction to propose a new species.
 *
 * @param proposerPublicKey - Wallet address proposing the species
 * @param slug - Short identifier (e.g., "mahogany")
 * @param name - Human-readable name
 * @param co2_scaled - kg CO₂/year × 100
 * @param maturity_years - Years to biomass maturity
 * @param network - "testnet" | "mainnet"
 *
 * @returns Unsigned transaction XDR ready for signing
 */
export async function buildProposeSpeciesTransaction(
  _proposerPublicKey: string,
  _slug: string,
  _name: string,
  co2_scaled: number,
  maturity_years: number,
  _network: NetworkType
): Promise<{ transactionXdr: string; networkPassphrase: string }> {
  if (co2_scaled <= 0) {
    throw new Error('co2_scaled must be positive');
  }
  if (maturity_years === 0) {
    throw new Error('maturity_years must be > 0');
  }

  // TODO: Implement species proposal submission using Soroban contract client
  // This would create a transaction that calls the propose_species function
  throw new Error('Species proposal submission not yet implemented');
}

/**
 * Build a transaction to vote on a proposal.
 *
 * @param voterPublicKey - Wallet address voting
 * @param proposalId - Proposal ID to vote on
 * @param voteFor - true to vote for, false to vote against
 * @param network - "testnet" | "mainnet"
 *
 * @returns Unsigned transaction XDR ready for signing
 */
export async function buildVoteTransaction(
  _voterPublicKey: string,
  _proposalId: number,
  _voteFor: boolean,
  _network: NetworkType
): Promise<{ transactionXdr: string; networkPassphrase: string }> {
  // TODO: Implement voting submission using Soroban contract client
  // This would create a transaction that calls the vote function
  throw new Error('Voting submission not yet implemented');
}

/**
 * Build a transaction to execute a passed proposal.
 *
 * @param executorPublicKey - Wallet address executing the proposal
 * @param proposalId - Proposal ID to execute
 * @param network - "testnet" | "mainnet"
 *
 * @returns Unsigned transaction XDR ready for signing
 */
export async function buildExecuteProposalTransaction(
  _executorPublicKey: string,
  _proposalId: number,
  _network: NetworkType
): Promise<{ transactionXdr: string; networkPassphrase: string }> {
  // TODO: Implement proposal execution using Soroban contract client
  // This would create a transaction that calls the execute_proposal function
  throw new Error('Proposal execution not yet implemented');
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/**
 * Calculate the percentage of votes in favor.
 */
export function calculateVotePercentage(votesFor: number, votesAgainst: number): number {
  const total = votesFor + votesAgainst;
  if (total === 0) return 0;
  return (votesFor / total) * 100;
}

/**
 * Check if a proposal is still within its voting period.
 */
export function isVotingActive(votingEndsAt: number): boolean {
  return Date.now() / 1000 < votingEndsAt;
}

/**
 * Format the remaining voting time.
 */
export function formatVotingTimeRemaining(votingEndsAt: number): string {
  const now = Date.now() / 1000;
  const remaining = votingEndsAt - now;

  if (remaining <= 0) return 'Voting ended';

  const days = Math.floor(remaining / 86400);
  const hours = Math.floor((remaining % 86400) / 3600);

  if (days > 0) return `${days} day${days > 1 ? 's' : ''} remaining`;
  if (hours > 0) return `${hours} hour${hours > 1 ? 's' : ''} remaining`;
  return 'Less than 1 hour remaining';
}
