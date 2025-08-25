// No imports needed in Solana Playground: web3, anchor, pg, BN are globally available
// Declarations to satisfy TS in Playground:
declare const splToken: any; // provided by Playground env at runtime
/* eslint-disable @typescript-eslint/no-explicit-any */

describe("construction_escrow end-to-end", () => {
  const PROGRAM_ID = pg.program.programId;

  // ---- handy helpers -------------------------------------------------------
  const LAMPORTS = 1_000_000_000;
  const WALLET = pg.wallet.publicKey;
  // Anchor wallet has `payer` (Keypair) at runtime; TS doesn’t know it here, so cast:
  const PAYER: any = (pg.wallet as any).payer;

  async function airdrop(pubkey: web3.PublicKey, sol = 2) {
    const sig = await pg.connection.requestAirdrop(pubkey, sol * LAMPORTS);
    await pg.connection.confirmTransaction(sig, "confirmed");
  }

  async function logOnErr<T>(label: string, fn: () => Promise<T>): Promise<T> {
    try {
      return await fn();
    } catch (e: any) {
      if (e?.logs) console.error(`[${label}] Logs:`, e.logs);
      const sig = e?.signature || e?.txid;
      if (sig) {
        const tx = await pg.connection.getTransaction(sig, {
          commitment: "confirmed",
          maxSupportedTransactionVersion: 0,
        });
        console.error(`[${label}] getTransaction logs:`, JSON.stringify(tx?.meta?.logMessages, null, 2));
      }
      console.error(`[${label}] Error:`, e);
      throw e;
    }
  }

  function u64(n: number | string | BN) {
    return new BN(n.toString());
  }

  // Encode BN as 8-byte big-endian buffer WITHOUT bigint APIs
  function buf8FromU64(n: BN) {
    return Buffer.from(n.toArray("be", 8));
  }

  // PDA helpers (must match lib.rs seeds exactly)
  function pdaConfig() {
    return web3.PublicKey.findProgramAddressSync([Buffer.from("config")], PROGRAM_ID);
  }
  function pdaProjectIndex(projectIdBN: BN) {
    return web3.PublicKey.findProgramAddressSync(
      [Buffer.from("project_index"), buf8FromU64(projectIdBN)],
      PROGRAM_ID
    );
  }
  function pdaEscrow(projectIdBN: BN, buyer: web3.PublicKey, seller: web3.PublicKey, mint: web3.PublicKey) {
    return web3.PublicKey.findProgramAddressSync(
      [Buffer.from("escrow"), buf8FromU64(projectIdBN), buyer.toBuffer(), seller.toBuffer(), mint.toBuffer()],
      PROGRAM_ID
    );
  }
  function pdaVaultAuthority(escrow: web3.PublicKey) {
    return web3.PublicKey.findProgramAddressSync([Buffer.from("vault"), escrow.toBuffer()], PROGRAM_ID);
  }

  async function getLogs(sig: string) {
    const tx = await pg.connection.getTransaction(sig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    return tx?.meta?.logMessages || [];
  }

  // ---- test body -----------------------------------------------------------
  it("happy path: config → create_escrow → verify → milestone → release → final release", async () => {
    // ----- bootstrap SPL mint and ATAs -------------------------------------
    const mintKp = web3.Keypair.generate();
    const sellerKp = web3.Keypair.generate();
    const treasuryKp = web3.Keypair.generate();
    const insuranceKp = web3.Keypair.generate();
    const arbiterKp = web3.Keypair.generate();

    // Two oracles for quorum=1 (M-of-N)
    const oracle1 = web3.Keypair.generate();
    const oracle2 = web3.Keypair.generate();

    // Airdrops for fees/rent
    await airdrop(WALLET);
    await airdrop(sellerKp.publicKey);
    await airdrop(treasuryKp.publicKey);
    await airdrop(insuranceKp.publicKey);
    await airdrop(arbiterKp.publicKey);
    await airdrop(oracle1.publicKey);
    await airdrop(oracle2.publicKey);

    // Create Mint (6 decimals), mint to buyer
    const decimals = 6;
    const mintRent = await splToken.getMinimumBalanceForRentExemptMint(pg.connection);
    const createMintIx = web3.SystemProgram.createAccount({
      fromPubkey: WALLET,
      newAccountPubkey: mintKp.publicKey,
      lamports: mintRent,
      space: splToken.MintLayout.span,
      programId: splToken.TOKEN_PROGRAM_ID,
    });
    const initMintIx = splToken.createInitializeMintInstruction(
      mintKp.publicKey,
      decimals,
      WALLET,         // mint authority
      WALLET          // freeze authority
    );

    // Build & send tx to create mint
    {
      const tx = new web3.Transaction().add(createMintIx, initMintIx);
      tx.feePayer = WALLET;
      const sig = await web3.sendAndConfirmTransaction(pg.connection, tx, [PAYER, mintKp]);
      console.log("Mint created:", sig);
    }

    // Derive ATAs
    const buyerAta = await splToken.getAssociatedTokenAddress(
      mintKp.publicKey, WALLET, false, splToken.TOKEN_PROGRAM_ID, splToken.ASSOCIATED_TOKEN_PROGRAM_ID
    );
    const sellerAta = await splToken.getAssociatedTokenAddress(mintKp.publicKey, sellerKp.publicKey);
    const treasuryAta = await splToken.getAssociatedTokenAddress(mintKp.publicKey, treasuryKp.publicKey);
    const insuranceAta = await splToken.getAssociatedTokenAddress(mintKp.publicKey, insuranceKp.publicKey);

    // Ensure ATAs exist
    const createBuyerAtaIx = splToken.createAssociatedTokenAccountInstruction(
      WALLET, buyerAta, WALLET, mintKp.publicKey
    );
    const createSellerAtaIx = splToken.createAssociatedTokenAccountInstruction(
      WALLET, sellerAta, sellerKp.publicKey, mintKp.publicKey
    );
    const createTreasuryAtaIx = splToken.createAssociatedTokenAccountInstruction(
      WALLET, treasuryAta, treasuryKp.publicKey, mintKp.publicKey
    );
    const createInsuranceAtaIx = splToken.createAssociatedTokenAccountInstruction(
      WALLET, insuranceAta, insuranceKp.publicKey, mintKp.publicKey
    );

    {
      const tx = new web3.Transaction().add(
        createBuyerAtaIx, createSellerAtaIx, createTreasuryAtaIx, createInsuranceAtaIx
      );
      try {
        const sig = await web3.sendAndConfirmTransaction(pg.connection, tx, [PAYER]);
        console.log("ATAs created:", sig);
      } catch (e:any) {
        if (!`${e}`.includes("already in use")) throw e;
        console.log("ATAs already existed, continuing");
      }
    }

    // Mint tokens to buyer ATA (use plain number ONLY)
    {
      const mintAmount: number = 1_000_000_000; // 1,000 tokens with 6 decimals
      const mintIx = splToken.createMintToInstruction(
        mintKp.publicKey, buyerAta, WALLET, mintAmount
      );
      const sig = await web3.sendAndConfirmTransaction(pg.connection, new web3.Transaction().add(mintIx), [PAYER]);
      console.log("Buyer funded with tokens:", sig);
    }

    // ----- derive PDAs that match lib.rs -----------------------------------
    const projectId = u64(1234);
    const [configPda] = pdaConfig();
    const [projectIndexPda] = pdaProjectIndex(projectId);
    const [escrowPda] = pdaEscrow(projectId, WALLET, sellerKp.publicKey, mintKp.publicKey);
    const [vaultAuthPda] = pdaVaultAuthority(escrowPda);
    const vaultAta = await splToken.getAssociatedTokenAddress(mintKp.publicKey, vaultAuthPda, true);

    console.log("Config PDA:", configPda.toBase58());
    console.log("Escrow PDA:", escrowPda.toBase58());
    console.log("Vault Auth PDA:", vaultAuthPda.toBase58());
    console.log("Vault ATA:", vaultAta.toBase58());
    console.log("ProjectIndex PDA:", projectIndexPda.toBase58());

    // Defensive existence checks (before calling into program)
    {
      const mintAcc = await pg.connection.getAccountInfo(mintKp.publicKey);
      if (!mintAcc) throw new Error("Mint account missing");
      const buyerAtaAcc = await pg.connection.getAccountInfo(buyerAta);
      if (!buyerAtaAcc) throw new Error("Buyer ATA missing");
    }

    // ----- init_config (fee/insurance/retention) ---------------------------
    const feeBps = 100;       // 1%
    const insuranceBps = 50;  // 0.5%
    const retentionBps = 500; // 5%
    const warrantyDays = new BN(0); // allow immediate retention release for test
    const quorumM = 1;

    await logOnErr("init_config", async () => {
      const info = await pg.connection.getAccountInfo(configPda);
      if (info) {
        console.log("Config already initialized; skipping init");
        return;
      }
      const sig = await pg.program.methods
        .initConfig(feeBps, insuranceBps, retentionBps, warrantyDays, quorumM)
        .accounts({
          authority: WALLET,
          treasury: treasuryKp.publicKey,
          insuranceTreasury: insuranceKp.publicKey,
          arbiter: arbiterKp.publicKey,
          config: configPda,
          systemProgram: web3.SystemProgram.programId,
        })
        .signers([PAYER])
        .rpc();
      console.log("init_config sig:", sig);
      console.log("init_config logs:", await getLogs(sig));
    });

    // ----- create_escrow ---------------------------------------------------
    const amount = u64(100_000_000); // 100 tokens (6 decimals)
    const ixNonce = u64(Date.now()); // idempotency key
    const oracles: web3.PublicKey[] = [oracle1.publicKey, oracle2.publicKey];
    const priceSnapshot = u64(10_000_000); // 10.000000 USD for example
    const nftEnabled = false;

    await logOnErr("create_escrow", async () => {
      const sig = await pg.program.methods
        .createEscrow(
          projectId,
          amount,
          ixNonce,
          oracles,
          quorumM,
          priceSnapshot,
          nftEnabled
        )
        .accounts({
          buyer: WALLET,
          seller: sellerKp.publicKey,
          mint: mintKp.publicKey,
          buyerAta,
          escrow: escrowPda,
          projectIndex: projectIndexPda,
          vaultAuthority: vaultAuthPda,
          vaultAta,
          config: configPda,
          tokenProgram: splToken.TOKEN_PROGRAM_ID,
          associatedTokenProgram: splToken.ASSOCIATED_TOKEN_PROGRAM_ID,
          systemProgram: web3.SystemProgram.programId,
          rent: web3.SYSVAR_RENT_PUBKEY,
        })
        .signers([PAYER])
        .rpc();

      console.log("create_escrow sig:", sig);
      console.log("create_escrow logs:", await getLogs(sig));
    });

    // Validate escrow state after creation
    const escrowAccAfterCreate = await pg.program.account.escrow.fetch(escrowPda);
    console.log("escrow after create:", {
      state: escrowAccAfterCreate.state,
      amount: escrowAccAfterCreate.amount.toString(),
      buyer: escrowAccAfterCreate.buyer.toBase58(),
      seller: escrowAccAfterCreate.seller.toBase58(),
      mint: escrowAccAfterCreate.mint.toBase58(),
    });
    assert.equal(escrowAccAfterCreate.amount.toString(), amount.toString());
    assert.equal(escrowAccAfterCreate.state, 1 /* Open */);

    // ----- set deadlines (verify_by_ts + deliver_by_ts) --------------------
    const now = Math.floor(Date.now() / 1000);
    await logOnErr("set_deadlines", async () => {
      const sig = await pg.program.methods
        .setDeadlines(new BN(now + 60), new BN(now + 120))
        .accounts({
          actor: WALLET,
          escrow: escrowPda,
        })
        .signers([PAYER])
        .rpc();
      console.log("set_deadlines sig:", sig);
      console.log("set-deadlines logs:", await getLogs(sig));
    });

    // ----- add a milestone --------------------------------------------------
    const milestoneAmount = u64(40_000_000); // 40 tokens
    const evHashU8 = new Uint8Array(32); // zeroed test hash
    const evHashNumArr = Array.from(evHashU8); // convert to number[]

    await logOnErr("add_milestone", async () => {
      const sig = await pg.program.methods
        .addMilestone(milestoneAmount, evHashNumArr)
        .accounts({
          actor: WALLET,
          escrow: escrowPda,
        })
        .signers([PAYER])
        .rpc();
      console.log("add_milestone sig:", sig);
      console.log("add_milestone logs:", await getLogs(sig));
    });

    // Validate milestone added
    {
      const e = await pg.program.account.escrow.fetch(escrowPda);
      assert.equal(e.milestonesLen, 1);
      assert.equal(e.milestones[0].amount.toString(), milestoneAmount.toString());
      assert.equal(e.milestones[0].verified, false);
    }

    // ----- verify milestone with quorum (1 of 2 oracles) -------------------
    await logOnErr("verify_milestone", async () => {
      const sig = await pg.program.methods
        .verifyMilestone(0)
        .accounts({ escrow: escrowPda })
        .remainingAccounts([{ pubkey: oracle1.publicKey, isSigner: true, isWritable: false }])
        .signers([oracle1])
        .rpc();
      console.log("verify_milestone sig:", sig);
      console.log("verify_milestone logs:", await getLogs(sig));
    });

    // Validate milestone verified
    {
      const e = await pg.program.account.escrow.fetch(escrowPda);
      assert.equal(e.milestones[0].verified, true);
      assert.ok(e.state === 2 || e.state === 3); // Verified or PartiallyReleased
    }

    // ----- release_for_milestone (routes fees & insurance) -----------------
    await logOnErr("release_for_milestone", async () => {
      const sig = await pg.program.methods
        .releaseForMilestone(0)
        .accounts({
          escrow: escrowPda,
          vaultAuthority: vaultAuthPda,
          vaultAta,
          sellerAta,
          buyerAta,          // for late penalty (unused here)
          treasuryAta,
          insuranceAta,
          tokenProgram: splToken.TOKEN_PROGRAM_ID,
        })
        .rpc();
      console.log("release_for_milestone sig:", sig);
      console.log("release_for_milestone logs:", await getLogs(sig));
    });

    // Validate balances roughly (40 tokens - fees)
    {
      const sellerAcc = await splToken.getAccount(pg.connection, sellerAta);
      const treasuryAcc = await splToken.getAccount(pg.connection, treasuryAta);
      const insuranceAcc = await splToken.getAccount(pg.connection, insuranceAta);
      const vaultAcc = await splToken.getAccount(pg.connection, vaultAta);

      const fee = Math.floor(Number(milestoneAmount) * feeBps / 10_000);
      const ins = Math.floor(Number(milestoneAmount) * insuranceBps / 10_000);
      const sellerRecv = Number(milestoneAmount) - fee - ins;

      console.log("post milestone release balances:", {
        seller: Number(sellerAcc.amount),
        treasury: Number(treasuryAcc.amount),
        insurance: Number(insuranceAcc.amount),
        vault: Number(vaultAcc.amount),
      });

      assert.equal(Number(treasuryAcc.amount), fee);
      assert.equal(Number(insuranceAcc.amount), ins);
      assert.equal(Number(sellerAcc.amount), sellerRecv);
      assert.ok(Number(vaultAcc.amount) <= Number(amount) - Number(milestoneAmount) + 1);
    }

    // ----- verify_delivery (overall) ---------------------------------------
    await logOnErr("verify_delivery", async () => {
      const sig = await pg.program.methods
        .verifyDelivery(projectId)
        .accounts({ escrow: escrowPda })
        .remainingAccounts([{ pubkey: oracle1.publicKey, isSigner: true, isWritable: false }])
        .signers([oracle1])
        .rpc();
      console.log("verify_delivery sig:", sig);
      console.log("verify_delivery logs:", await getLogs(sig));
    });

    // ----- release_payment (remaining, keep retention in vault) ------------
    await logOnErr("release_payment", async () => {
      const sig = await pg.program.methods
        .releasePayment()
        .accounts({
          escrow: escrowPda,
          vaultAuthority: vaultAuthPda,
          vaultAta,
          sellerAta,
          buyerAta,
          treasuryAta,
          insuranceAta,
          tokenProgram: splToken.TOKEN_PROGRAM_ID,
        })
        .rpc();
      console.log("release_payment sig:", sig);
      console.log("release_payment logs:", await getLogs(sig));
    });

    // Validate escrow state Released (4) but retention still in vault
    {
      const e = await pg.program.account.escrow.fetch(escrowPda);
      assert.equal(e.state, 4 /* Released */);
      const v = await splToken.getAccount(pg.connection, vaultAta);
      const retention = Math.floor(Number(e.amount) * retentionBps / 10_000);
      console.log("vault after release_payment (should hold retention):", Number(v.amount), "expected≈", retention);
      assert.ok(Math.abs(Number(v.amount) - retention) <= 2);
    }

    // ----- release_retention (warrantyDays=0 so it’s immediately allowed) --
    await logOnErr("release_retention", async () => {
      const sig = await pg.program.methods
        .releaseRetention()
        .accounts({
          escrow: escrowPda,
          vaultAuthority: vaultAuthPda,
          vaultAta,
          sellerAta,
          buyerAta, // unused here
          treasuryAta,
          insuranceAta,
          tokenProgram: splToken.TOKEN_PROGRAM_ID,
        })
        .rpc();
      console.log("release_retention sig:", sig);
      console.log("release_retention logs:", await getLogs(sig));
    });

    // Validate retention drained to seller (minus fees/insurance again)
    {
      const v = await splToken.getAccount(pg.connection, vaultAta);
      console.log("vault after retention release:", Number(v.amount));
      assert.equal(Number(v.amount), 0);

      const e = await pg.program.account.escrow.fetch(escrowPda);
      assert.equal(e.retentionReleased, true);
    }

    console.log("✅ happy-path test completed");
  });

  it("defensive: shows failure logs if releasing before verification", async () => {
    const sellerKp = web3.Keypair.generate();
    await airdrop(sellerKp.publicKey);

    const mint = web3.Keypair.generate();
    const mintRent = await splToken.getMinimumBalanceForRentExemptMint(pg.connection);
    const tx = new web3.Transaction().add(
      web3.SystemProgram.createAccount({
        fromPubkey: WALLET,
        newAccountPubkey: mint.publicKey,
        lamports: mintRent,
        space: splToken.MintLayout.span,
        programId: splToken.TOKEN_PROGRAM_ID,
      }),
      splToken.createInitializeMintInstruction(mint.publicKey, 6, WALLET, WALLET)
    );
    await web3.sendAndConfirmTransaction(pg.connection, tx, [PAYER, mint]);

    const buyerAta = await splToken.getAssociatedTokenAddress(mint.publicKey, WALLET);
    const sellerAta = await splToken.getAssociatedTokenAddress(mint.publicKey, sellerKp.publicKey);
    // ensure ATAs
    try {
      await web3.sendAndConfirmTransaction(
        pg.connection,
        new web3.Transaction().add(
          splToken.createAssociatedTokenAccountInstruction(WALLET, buyerAta, WALLET, mint.publicKey),
          splToken.createAssociatedTokenAccountInstruction(WALLET, sellerAta, sellerKp.publicKey, mint.publicKey),
        ),
        [PAYER]
      );
    } catch (e:any) {
      if (!`${e}`.includes("already in use")) throw e;
    }
    // mint buyer funds (use number only)
    await web3.sendAndConfirmTransaction(
      pg.connection,
      new web3.Transaction().add(
        splToken.createMintToInstruction(mint.publicKey, buyerAta, WALLET, 500_000_000) // 500 tokens (6 dp)
      ),
      [PAYER]
    );

    const projectId = u64(9999);
    const [configPda] = pdaConfig();
    const [escrowPda] = pdaEscrow(projectId, WALLET, sellerKp.publicKey, mint.publicKey);
    const [vaultAuth] = pdaVaultAuthority(escrowPda);
    const vaultAta = await splToken.getAssociatedTokenAddress(mint.publicKey, vaultAuth, true);
    const [projectIndexPda] = pdaProjectIndex(projectId);

    const treasuryKp = web3.Keypair.generate();
    const insuranceKp = web3.Keypair.generate();
    await airdrop(treasuryKp.publicKey);
    await airdrop(insuranceKp.publicKey);
    const treasuryAta = await splToken.getAssociatedTokenAddress(mint.publicKey, treasuryKp.publicKey);
    const insuranceAta = await splToken.getAssociatedTokenAddress(mint.publicKey, insuranceKp.publicKey);
    try {
      await web3.sendAndConfirmTransaction(
        pg.connection,
        new web3.Transaction().add(
          splToken.createAssociatedTokenAccountInstruction(WALLET, treasuryAta, treasuryKp.publicKey, mint.publicKey),
          splToken.createAssociatedTokenAccountInstruction(WALLET, insuranceAta, insuranceKp.publicKey, mint.publicKey),
        ),
        [PAYER]
      );
    } catch (e:any) {
      if (!`${e}`.includes("already in use")) throw e;
    }

    // Ensure config exists (if previous test ran, it's there)
    const cfgAcc = await pg.connection.getAccountInfo(configPda);
    if (!cfgAcc) {
      const sig = await pg.program.methods
        .initConfig(100, 50, 500, new BN(0), 1)
        .accounts({
          authority: WALLET,
          treasury: treasuryKp.publicKey,
          insuranceTreasury: insuranceKp.publicKey,
          arbiter: WALLET,
          config: configPda,
          systemProgram: web3.SystemProgram.programId,
        })
        .signers([PAYER])
        .rpc();
      await pg.connection.confirmTransaction(sig, "confirmed");
    }

    // Create escrow with no verification yet
    await pg.program.methods
      .createEscrow(projectId, u64(200_000_000), u64(Date.now()), [], 1, u64(0), false)
      .accounts({
        buyer: WALLET,
        seller: sellerKp.publicKey,
        mint: mint.publicKey,
        buyerAta,
        escrow: escrowPda,
        projectIndex: projectIndexPda,
        vaultAuthority: vaultAuth,
        vaultAta,
        config: configPda,
        tokenProgram: splToken.TOKEN_PROGRAM_ID,
        associatedTokenProgram: splToken.ASSOCIATED_TOKEN_PROGRAM_ID,
        systemProgram: web3.SystemProgram.programId,
        rent: web3.SYSVAR_RENT_PUBKEY,
      })
      .signers([PAYER])
      .rpc();

    // Try (and fail) to release_payment before verification
    let failed = false;
    try {
      await pg.program.methods
        .releasePayment()
        .accounts({
          escrow: escrowPda,
          vaultAuthority: vaultAuth,
          vaultAta,
          sellerAta,
          buyerAta,
          treasuryAta,
          insuranceAta,
          tokenProgram: splToken.TOKEN_PROGRAM_ID,
        })
        .rpc();
    } catch (e:any) {
      failed = true;
      console.log("Expected failure (release before verify). Error:", e.error ? e.error : e.toString());
      if (e?.signature || e?.txid) {
        const logs = await getLogs(e.signature || e.txid);
        console.log("Failure logs:", logs);
      }
    }
    assert.ok(failed, "release_payment should fail before verification");
  });
});
