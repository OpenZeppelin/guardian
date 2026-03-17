import { useState, useEffect, useRef } from 'react';
import { useParaMiden } from '@miden-sdk/use-miden-para-react';
import { tryComputeEcdsaCommitmentHex, EcdsaFormat } from '@openzeppelin/miden-multisig-client';
import { MIDEN_RPC_URL } from '@/config';
import type { ExternalWalletState } from '@/wallets/types';

interface WalletWithPublicKey {
  id: string;
  publicKey?: string;
}

async function getUncompressedPublicKeyFromWallet(
  para: { issueJwt(): Promise<{ token: string }> },
  wallet: WalletWithPublicKey,
): Promise<string> {
  let publicKey = wallet.publicKey;
  if (!publicKey) {
    const { token } = await para.issueJwt();
    const payload = JSON.parse(window.atob(token.split('.')[1]));
    if (!payload.data) {
      throw new Error('Got invalid jwt token');
    }
    const wallets: Array<{ id: string; publicKey: string }> = payload.data.connectedWallets;
    const matchingWallet = wallets.find((entry) => entry.id === wallet.id);
    if (!matchingWallet) {
      throw new Error('Wallet Not Found in jwt data');
    }
    publicKey = matchingWallet.publicKey;
  }
  return publicKey;
}

export function useParaSession() {
  const [session, setSession] = useState<ExternalWalletState>({
    source: 'para',
    connected: false,
    publicKey: null,
    commitment: null,
    scheme: null,
  });

  const paraMiden = useParaMiden(MIDEN_RPC_URL, 'public', {}, false);
  const derivingRef = useRef(false);

  const { para: paraClient, evmWallets } = paraMiden;
  const walletId = evmWallets?.[0]?.id ?? null;

  useEffect(() => {
    if (!evmWallets?.length) {
      setSession((prev) => ({ ...prev, connected: false, publicKey: null, commitment: null }));
      return;
    }

    const evmWallet = evmWallets[0];
    if (!paraClient || derivingRef.current) return;

    derivingRef.current = true;
    (async () => {
      try {
        if (!walletId) {
          throw new Error('Para wallet did not expose an id');
        }

        const uncompressedPublicKey = await getUncompressedPublicKeyFromWallet(
          paraClient,
          evmWallet,
        );
        const compressedPublicKey = EcdsaFormat.compressPublicKey(uncompressedPublicKey);
        const commitment = tryComputeEcdsaCommitmentHex(compressedPublicKey);
        if (!commitment) {
          throw new Error('Failed to derive ECDSA commitment from public key');
        }

        setSession({
          source: 'para',
          connected: true,
          publicKey: uncompressedPublicKey,
          commitment,
          scheme: 'ecdsa',
        });
      } catch {
        setSession((prev) => ({ ...prev, connected: false }));
      } finally {
        derivingRef.current = false;
      }
    })();
  }, [evmWallets, paraClient, walletId]);

  return {
    session,
    paraClient,
    paraMiden,
    walletId,
  };
}
