import {
  Account,
  Address,
  Contract,
  Keypair,
  nativeToScVal,
  rpc as SorobanRpc,
  TransactionBuilder,
  xdr,
} from "@stellar/stellar-sdk";

// ── Types ──────────────────────────────────────────────────────────────────────

interface CachedPrice {
  token: string;
  symbol: string;
  min: string; // i128 serialized as string
  max: string; // i128 serialized as string
  timestamp: number;
  ledger_seq: number;
  sources_used: string[];
  signature: string; // hex-encoded 64 bytes
}

interface Env {
  KEEPER_PRIVATE_KEY_HEX: string;
  ORACLE_WORKER_URL: string;
  ORACLE_SERVICE: Fetcher; // service binding to the oracle worker
  RPC_URL: string;
  ORACLE_CONTRACT: string;
  ORDER_HANDLER: string;
  DEPOSIT_HANDLER: string;
  WITHDRAWAL_HANDLER: string;
  READER_CONTRACT: string;
  DATA_STORE: string;
  NETWORK_PASSPHRASE: string;
}

// ── Worker export ──────────────────────────────────────────────────────────────

export default {
  async scheduled(_event: ScheduledEvent, env: Env, ctx: ExecutionContext): Promise<void> {
    ctx.waitUntil(runKeeperCycle(env).catch(console.error));
  },

  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/run") {
      try {
        const result = await runKeeperCycle(env);
        return Response.json(result);
      } catch (e: unknown) {
        return Response.json({ error: String(e) }, { status: 500 });
      }
    }
    return Response.json({ status: "ok", hint: "GET /run to trigger manually" });
  },
};

// ── Main keeper cycle ──────────────────────────────────────────────────────────

async function runKeeperCycle(env: Env): Promise<object> {
  const logs: string[] = [];
  const log = (msg: string) => {
    console.log(msg);
    logs.push(msg);
  };

  const keypair = Keypair.fromRawEd25519Seed(
    Buffer.from(env.KEEPER_PRIVATE_KEY_HEX, "hex"),
  );
  const server = new SorobanRpc.Server(env.RPC_URL, { allowHttp: false });

  // 1. Fetch signed prices from oracle worker via service binding
  log("Fetching prices from oracle service binding");
  const pricesResp = await env.ORACLE_SERVICE.fetch("https://oracle.internal/prices");
  if (!pricesResp.ok) {
    throw new Error(`Oracle prices fetch failed: ${pricesResp.status} ${await pricesResp.text()}`);
  }
  const prices: CachedPrice[] = await pricesResp.json() as CachedPrice[];
  if (prices.length === 0) throw new Error("Oracle returned empty price list");
  log(`Got ${prices.length} prices: ${prices.map((p) => p.symbol).join(", ")}`);

  // 2. Get all pending keys
  const orderKeys    = await getOrderKeys(server, env, log);
  const depositKeys  = await getDepositKeys(server, env, log);
  const withdrawalKeys = await getWithdrawalKeys(server, env, log);

  log(`Found ${orderKeys.length} orders, ${depositKeys.length} deposits, ${withdrawalKeys.length} withdrawals`);

  if (orderKeys.length === 0 && depositKeys.length === 0 && withdrawalKeys.length === 0) {
    return { status: "no_work", pricesAvailable: prices.length, logs };
  }

  // 3. Set prices on-chain (tx 1)
  log("Submitting set_prices...");
  await setPrices(server, keypair, prices, env, log);
  log("set_prices confirmed. Waiting for temp-storage to propagate...");
  await sleep(5000);

  // 4. Execute pending orders
  const orderResults: string[] = [];
  for (const orderKey of orderKeys) {
    try {
      log(`Executing order ${orderKey.slice(0, 8)}...`);
      const hash = await executeOrder(server, keypair, orderKey, env, log);
      orderResults.push(`${orderKey.slice(0, 8)}: OK tx=${hash.slice(0, 8)}`);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      log(`Order ${orderKey.slice(0, 8)} failed: ${msg}`);

      if (msg.includes("Budget, ExceededLimit")) {
        try {
          log(`Freezing budget-exceeded order ${orderKey.slice(0, 8)}...`);
          await freezeOrder(server, keypair, orderKey, env, log);
          orderResults.push(`${orderKey.slice(0, 8)}: FROZEN (budget exceeded)`);
        } catch (fe: unknown) {
          orderResults.push(`${orderKey.slice(0, 8)}: FAIL+FREEZE_ERR (${msg.slice(0, 60)})`);
        }
      } else {
        orderResults.push(`${orderKey.slice(0, 8)}: FAIL (${msg.slice(0, 80)})`);
      }
    }
  }

  // 5. Execute pending deposits
  const depositResults: string[] = [];
  for (const depositKey of depositKeys) {
    try {
      log(`Executing deposit ${depositKey.slice(0, 8)}...`);
      const hash = await executeDeposit(server, keypair, depositKey, env, log);
      depositResults.push(`${depositKey.slice(0, 8)}: OK tx=${hash.slice(0, 8)}`);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      log(`Deposit ${depositKey.slice(0, 8)} failed: ${msg}`);
      depositResults.push(`${depositKey.slice(0, 8)}: FAIL (${msg.slice(0, 80)})`);
    }
  }

  // 6. Execute pending withdrawals
  const withdrawalResults: string[] = [];
  for (const withdrawalKey of withdrawalKeys) {
    try {
      log(`Executing withdrawal ${withdrawalKey.slice(0, 8)}...`);
      const hash = await executeWithdrawal(server, keypair, withdrawalKey, env, log);
      withdrawalResults.push(`${withdrawalKey.slice(0, 8)}: OK tx=${hash.slice(0, 8)}`);
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      log(`Withdrawal ${withdrawalKey.slice(0, 8)} failed: ${msg}`);
      withdrawalResults.push(`${withdrawalKey.slice(0, 8)}: FAIL (${msg.slice(0, 80)})`);
    }
  }

  return {
    status: "done",
    pricesSet: prices.length,
    orders: orderResults,
    deposits: depositResults,
    withdrawals: withdrawalResults,
    logs,
  };
}

// ── Read pending order keys from reader contract ───────────────────────────────

async function getOrderKeys(
  server: SorobanRpc.Server,
  env: Env,
  log: (m: string) => void,
): Promise<string[]> {
  return getPendingKeys(server, env, log, "get_order_count", "get_order_keys");
}

// ── Read pending deposit keys ──────────────────────────────────────────────────

async function getDepositKeys(
  server: SorobanRpc.Server,
  env: Env,
  log: (m: string) => void,
): Promise<string[]> {
  return getPendingKeys(server, env, log, "get_deposit_count", "get_deposit_keys");
}

// ── Read pending withdrawal keys ───────────────────────────────────────────────

async function getWithdrawalKeys(
  server: SorobanRpc.Server,
  env: Env,
  log: (m: string) => void,
): Promise<string[]> {
  return getPendingKeys(server, env, log, "get_withdrawal_count", "get_withdrawal_keys");
}

// ── Shared helper for count+keys reader pattern ────────────────────────────────

async function getPendingKeys(
  server: SorobanRpc.Server,
  env: Env,
  log: (m: string) => void,
  countMethod: string,
  keysMethod: string,
): Promise<string[]> {
  const contract = new Contract(env.READER_CONTRACT);
  const dummy = new Account("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF", "0");

  const countTx = new TransactionBuilder(dummy, {
    fee: "100",
    networkPassphrase: env.NETWORK_PASSPHRASE,
  })
    .addOperation(contract.call(countMethod, new Address(env.DATA_STORE).toScVal()))
    .setTimeout(10)
    .build();

  const countSim = await server.simulateTransaction(countTx);
  if (SorobanRpc.Api.isSimulationError(countSim)) {
    log(`${countMethod} simulation error: ${countSim.error}`);
    return [];
  }
  const countRetval = (countSim as SorobanRpc.Api.SimulateTransactionSuccessResponse).result?.retval;
  if (!countRetval) return [];

  const count: number = countRetval.u32();
  if (count === 0) return [];

  const dummy2 = new Account("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF", "0");
  const keysTx = new TransactionBuilder(dummy2, {
    fee: "100",
    networkPassphrase: env.NETWORK_PASSPHRASE,
  })
    .addOperation(
      contract.call(
        keysMethod,
        new Address(env.DATA_STORE).toScVal(),
        xdr.ScVal.scvU32(0),
        xdr.ScVal.scvU32(count),
      ),
    )
    .setTimeout(10)
    .build();

  const keysSim = await server.simulateTransaction(keysTx);
  if (SorobanRpc.Api.isSimulationError(keysSim)) {
    log(`${keysMethod} simulation error: ${keysSim.error}`);
    return [];
  }
  const keysRetval = (keysSim as SorobanRpc.Api.SimulateTransactionSuccessResponse).result?.retval;
  if (!keysRetval) return [];

  const vec = keysRetval.vec() ?? [];
  return vec
    .map((v) => {
      try {
        const bytes = v.bytes();
        return bytes ? Buffer.from(bytes).toString("hex") : null;
      } catch {
        return null;
      }
    })
    .filter((x): x is string => x !== null);
}

// ── Submit execute_deposit transaction ─────────────────────────────────────────

async function executeDeposit(
  server: SorobanRpc.Server,
  keypair: Keypair,
  depositKey: string,
  env: Env,
  log: (m: string) => void,
): Promise<string> {
  return executeHandlerKey(server, keypair, depositKey, env.DEPOSIT_HANDLER, "execute_deposit", env, log);
}

// ── Submit execute_withdrawal transaction ──────────────────────────────────────

async function executeWithdrawal(
  server: SorobanRpc.Server,
  keypair: Keypair,
  withdrawalKey: string,
  env: Env,
  log: (m: string) => void,
): Promise<string> {
  return executeHandlerKey(server, keypair, withdrawalKey, env.WITHDRAWAL_HANDLER, "execute_withdrawal", env, log);
}

// ── Shared helper: call handler::execute_*(keeper, key) ────────────────────────

async function executeHandlerKey(
  server: SorobanRpc.Server,
  keypair: Keypair,
  key: string,
  contractId: string,
  methodName: string,
  env: Env,
  log: (m: string) => void,
): Promise<string> {
  const callerAddr = keypair.publicKey();
  const acct = await server.getAccount(callerAddr);
  const sorobanAcct = new Account(callerAddr, acct.sequenceNumber());

  const handlerContract = new Contract(contractId);
  const keyScVal = xdr.ScVal.scvBytes(Buffer.from(key, "hex"));

  const tx = new TransactionBuilder(sorobanAcct, {
    fee: "2000000",
    networkPassphrase: env.NETWORK_PASSPHRASE,
  })
    .addOperation(
      handlerContract.call(
        methodName,
        new Address(callerAddr).toScVal(),
        keyScVal,
      ),
    )
    .setTimeout(60)
    .build();

  const sim = await server.simulateTransaction(tx);
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw new Error(`${methodName} simulation failed: ${sim.error}`);
  }

  const assembled = SorobanRpc.assembleTransaction(tx, sim).build();
  assembled.sign(keypair);

  const submit = await server.sendTransaction(assembled);
  if (submit.status === "ERROR") {
    throw new Error(`${methodName} submit failed: ${JSON.stringify(submit.errorResult)}`);
  }
  log(`${methodName} hash: ${submit.hash}`);
  await pollTx(server, submit.hash, methodName, log);
  return submit.hash;
}

// ── Submit set_prices transaction ──────────────────────────────────────────────

async function setPrices(
  server: SorobanRpc.Server,
  keypair: Keypair,
  prices: CachedPrice[],
  env: Env,
  log: (m: string) => void,
): Promise<void> {
  const callerAddr = keypair.publicKey();
  const acct = await server.getAccount(callerAddr);
  const sorobanAcct = new Account(callerAddr, acct.sequenceNumber());

  const oracleContract = new Contract(env.ORACLE_CONTRACT);
  const signedPrices = xdr.ScVal.scvVec(prices.map(buildSignedPriceScVal));

  const tx = new TransactionBuilder(sorobanAcct, {
    fee: "1000000",
    networkPassphrase: env.NETWORK_PASSPHRASE,
  })
    .addOperation(
      oracleContract.call(
        "set_prices",
        new Address(callerAddr).toScVal(),
        signedPrices,
      ),
    )
    .setTimeout(30)
    .build();

  const sim = await server.simulateTransaction(tx);
  if (SorobanRpc.Api.isSimulationError(sim)) {
    throw new Error(`set_prices simulation failed: ${sim.error}`);
  }

  const assembled = SorobanRpc.assembleTransaction(tx, sim).build();
  assembled.sign(keypair);

  const submit = await server.sendTransaction(assembled);
  if (submit.status === "ERROR") {
    throw new Error(`set_prices submit failed: ${JSON.stringify(submit.errorResult)}`);
  }
  log(`set_prices hash: ${submit.hash}`);
  await pollTx(server, submit.hash, "set_prices", log);
}

// ── Submit execute_order transaction ───────────────────────────────────────────

async function executeOrder(
  server: SorobanRpc.Server,
  keypair: Keypair,
  orderKey: string,
  env: Env,
  log: (m: string) => void,
): Promise<string> {
  return executeHandlerKey(server, keypair, orderKey, env.ORDER_HANDLER, "execute_order", env, log);
}

// ── Freeze an order that exceeds compute budget ────────────────────────────────

async function freezeOrder(
  server: SorobanRpc.Server,
  keypair: Keypair,
  orderKey: string,
  env: Env,
  log: (m: string) => void,
): Promise<void> {
  await executeHandlerKey(server, keypair, orderKey, env.ORDER_HANDLER, "freeze_order", env, log);
}

// ── XDR helpers ────────────────────────────────────────────────────────────────

/**
 * Build a SignedPrice as a sorted ScMap (Soroban #[contracttype] struct encoding).
 *
 * Fields in alphabetical order (Soroban requires sorted ScMap keys):
 *   keeper_index, ledger_seq, max_price, min_price, signature, timestamp, token
 */
function buildSignedPriceScVal(p: CachedPrice): xdr.ScVal {
  const minPrice = BigInt(p.min);
  const maxPrice = BigInt(p.max);
  const sigBytes = Buffer.from(p.signature, "hex"); // must be 64 bytes

  return xdr.ScVal.scvMap([
    scEntry("keeper_index", nativeToScVal(0, { type: "u32" })),
    scEntry("ledger_seq",   nativeToScVal(p.ledger_seq, { type: "u32" })),
    scEntry("max_price",    nativeToScVal(maxPrice, { type: "i128" })),
    scEntry("min_price",    nativeToScVal(minPrice, { type: "i128" })),
    scEntry("signature",    nativeToScVal(sigBytes, { type: "bytes" })),
    scEntry("timestamp",    nativeToScVal(BigInt(p.timestamp), { type: "u64" })),
    scEntry("token",        new Address(p.token).toScVal()),
  ]);
}

function scEntry(key: string, val: xdr.ScVal): xdr.ScMapEntry {
  return new xdr.ScMapEntry({ key: xdr.ScVal.scvSymbol(key), val });
}

// ── Poll for tx confirmation ───────────────────────────────────────────────────

async function pollTx(
  server: SorobanRpc.Server,
  hash: string,
  label: string,
  log: (m: string) => void,
): Promise<void> {
  for (let i = 0; i < 20; i++) {
    await sleep(3000);
    const result = await server.getTransaction(hash);
    if (result.status === SorobanRpc.Api.GetTransactionStatus.SUCCESS) {
      log(`${label} confirmed (attempt ${i + 1})`);
      return;
    }
    if (result.status === SorobanRpc.Api.GetTransactionStatus.FAILED) {
      // Extract result XDR for diagnostics
      const meta = (result as SorobanRpc.Api.GetFailedTransactionResponse).resultMetaXdr;
      throw new Error(`${label} FAILED on-chain. hash=${hash.slice(0, 8)} meta=${meta?.toXDR("base64").slice(0, 100) ?? "n/a"}`);
    }
    log(`${label} pending (attempt ${i + 1}/20)`);
  }
  throw new Error(`${label} timed out after 60s: ${hash.slice(0, 8)}`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
