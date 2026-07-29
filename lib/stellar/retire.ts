import {
  Asset,
  Horizon,
  Memo,
  Networks,
  Operation,
  TransactionBuilder,
} from '@stellar/stellar-sdk';
import type { NetworkType } from '@/lib/types/wallet';
import type { RetirementSelection } from '@/lib/types/retire';

const MAX_MEMO_LENGTH = 28;

function getNetworkPassphrase(network: NetworkType): string {
  return network === 'mainnet' ? Networks.PUBLIC : Networks.TESTNET;
}

function getHorizonUrl(network: NetworkType): string {
  return network === 'mainnet'
    ? 'https://horizon.stellar.org'
    : 'https://horizon-testnet.stellar.org';
}

function buildRetirementMemo(idempotencyKey: string): string {
  const memo = `retire:${idempotencyKey}`;
  return memo.length > MAX_MEMO_LENGTH ? memo.slice(0, MAX_MEMO_LENGTH) : memo;
}

export async function buildRetirementTransaction(
  selection: RetirementSelection,
  sourcePublicKey: string,
  network: NetworkType,
  idempotencyKey: string
): Promise<{ transactionXdr: string; networkPassphrase: string }> {
  if (!selection.assetCode || !selection.issuer) {
    throw new Error('Missing asset information for retirement');
  }

  if (selection.quantity <= 0) {
    throw new Error('Retirement quantity must be greater than zero');
  }

  if (selection.quantity > selection.availableQuantity) {
    throw new Error('Retirement quantity exceeds available balance');
  }

  const networkPassphrase = getNetworkPassphrase(network);
  const server = new Horizon.Server(getHorizonUrl(network));
  const sourceAccount = await server.loadAccount(sourcePublicKey);

  const asset = new Asset(selection.assetCode, selection.issuer);

  // Burn by sending the asset back to issuer (common pattern for retirement/burn).
  const transaction = new TransactionBuilder(sourceAccount, {
    fee: '100',
    networkPassphrase,
  })
    .addOperation(
      Operation.payment({
        destination: selection.issuer,
        asset,
        amount: selection.quantity.toFixed(7),
      })
    )
    .addMemo(Memo.text(buildRetirementMemo(idempotencyKey)))
    .setTimeout(300)
    .build();

  return {
    transactionXdr: transaction.toXDR(),
    networkPassphrase,
  };
}
