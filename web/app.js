// Escrow Vault dApp frontend.
//
// Zero-build static app: talks to the Freighter wallet extension via the
// official `@stellar/freighter-api` package for connection/signing, and to
// `@stellar/stellar-sdk` for building, simulating, and submitting Soroban
// contract-invocation transactions against the escrow-vault contract. Both
// are loaded from a CDN as ES modules — no bundler, no node_modules.

// esm.sh only exposes these packages' exports correctly through a CJS-style
// `default` bundle rather than as named exports when loaded raw over a CDN
// (their plain ESM builds either fail to resolve internal subpaths, or lose
// their named exports) — hence the extra unwrap on both imports below.
// Verified directly against each package's published source before relying
// on this shape (see commit history / README for details).
const StellarSdkModule = await import("https://esm.sh/@stellar/stellar-sdk@12?bundle");
const StellarSdk = StellarSdkModule.default;

const FreighterApiModule = await import("https://esm.sh/@stellar/freighter-api");
const Freighter = FreighterApiModule.default;

const NETWORKS = {
  testnet: {
    label: "Testnet",
    rpcUrl: "https://soroban-testnet.stellar.org",
    passphrase: "Test SDF Network ; September 2015",
    explorerTx: (hash) => `https://stellar.expert/explorer/testnet/tx/${hash}`,
  },
  futurenet: {
    label: "Futurenet",
    rpcUrl: "https://rpc-futurenet.stellar.org",
    passphrase: "Test SDF Future Network ; October 2022",
    explorerTx: (hash) => `https://stellar.expert/explorer/futurenet/tx/${hash}`,
  },
  mainnet: {
    label: "Mainnet",
    rpcUrl: "https://mainnet.sorobanrpc.com",
    passphrase: "Public Global Stellar Network ; September 2015",
    explorerTx: (hash) => `https://stellar.expert/explorer/public/tx/${hash}`,
  },
};

const STATUS_LABELS = ["Pending", "Completed", "Refunded"];

const state = {
  address: null,
  network: localStorage.getItem("escrow.network") || "testnet",
  contractId: localStorage.getItem("escrow.contractId") || "",
};

// ---------------------------------------------------------------------
// DOM references
// ---------------------------------------------------------------------

const el = (id) => document.getElementById(id);

const connectBtn = el("connect-btn");
const walletAddressEl = el("wallet-address");
const networkSelect = el("network-select");
const contractIdInput = el("contract-id");
const saveSettingsBtn = el("save-settings-btn");

const initPayeeInput = el("init-payee");
const initArbiterInput = el("init-arbiter");
const initTokenInput = el("init-token");
const initAmountInput = el("init-amount");
const initBtn = el("init-btn");

const depositBtn = el("deposit-btn");
const releaseBtn = el("release-btn");
const refundBtn = el("refund-btn");
const refreshBtn = el("refresh-btn");

const statusBadge = el("status-badge");
const detailsList = el("escrow-details");
const activityLog = el("activity-log");

networkSelect.value = state.network;
contractIdInput.value = state.contractId;

// ---------------------------------------------------------------------
// Activity log
// ---------------------------------------------------------------------

function logEntry({ title, detail, hash, isError }) {
  const empty = activityLog.querySelector(".empty");
  if (empty) empty.remove();

  const wrap = document.createElement("div");
  wrap.className = `log-entry ${isError ? "error" : "ok"}`;

  const titleEl = document.createElement("div");
  titleEl.textContent = title;
  wrap.appendChild(titleEl);

  if (detail) {
    const detailEl = document.createElement("div");
    detailEl.className = "meta";
    detailEl.textContent = detail;
    wrap.appendChild(detailEl);
  }

  if (hash) {
    const linkEl = document.createElement("div");
    linkEl.className = "meta";
    const a = document.createElement("a");
    a.href = NETWORKS[state.network].explorerTx(hash);
    a.target = "_blank";
    a.rel = "noopener noreferrer";
    a.textContent = `View transaction: ${hash}`;
    linkEl.appendChild(a);
    wrap.appendChild(linkEl);
  }

  activityLog.prepend(wrap);
}

// ---------------------------------------------------------------------
// Wallet connection (Freighter)
// ---------------------------------------------------------------------

// Freighter's error results are `{ code, message, ext? }` objects, not
// plain strings — passing one straight to `new Error(...)` stringifies to
// "[object Object]". Unwrap `.message`, with a fallback in case a future
// API version changes the shape.
function freighterErrorMessage(error) {
  return (error && error.message) || String(error);
}

function truncate(address) {
  return address ? `${address.slice(0, 4)}…${address.slice(-4)}` : "";
}

function renderWallet() {
  walletAddressEl.textContent = state.address
    ? `Connected: ${truncate(state.address)}`
    : "Wallet not connected";
  connectBtn.textContent = state.address ? "Reconnect" : "Connect Freighter";
}

async function connectWallet() {
  try {
    const connected = await Freighter.isConnected();
    if (connected.error) throw new Error(freighterErrorMessage(connected.error));
    if (!connected.isConnected) {
      throw new Error(
        "Freighter extension not detected. Install it from freighter.app and reload this page."
      );
    }

    // requestAccess() both prompts for permission (if not already granted)
    // and returns the selected address in the same call.
    const access = await Freighter.requestAccess();
    if (access.error) throw new Error(freighterErrorMessage(access.error));

    state.address = access.address;
    renderWallet();
    logEntry({ title: "Wallet connected", detail: state.address });
  } catch (err) {
    logEntry({ title: "Failed to connect wallet", detail: err.message || String(err), isError: true });
  }
}

// ---------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------

function saveSettings() {
  state.network = networkSelect.value;
  state.contractId = contractIdInput.value.trim();
  localStorage.setItem("escrow.network", state.network);
  localStorage.setItem("escrow.contractId", state.contractId);
  logEntry({
    title: "Settings saved",
    detail: `Network: ${NETWORKS[state.network].label} — Contract: ${state.contractId || "(none)"}`,
  });
}

// ---------------------------------------------------------------------
// Soroban contract calls
// ---------------------------------------------------------------------

function requireContractId() {
  if (!state.contractId) {
    throw new Error("Set a Contract ID in Contract Settings first.");
  }
  return state.contractId;
}

function requireWallet() {
  if (!state.address) {
    throw new Error("Connect your Freighter wallet first.");
  }
  return state.address;
}

function getServer() {
  const rpc = StellarSdk.rpc || StellarSdk.SorobanRpc;
  return new rpc.Server(NETWORKS[state.network].rpcUrl, { allowHttp: false });
}

function addressArg(value) {
  return new StellarSdk.Address(value).toScVal();
}

function i128Arg(value) {
  return StellarSdk.nativeToScVal(BigInt(value), { type: "i128" });
}

/**
 * Builds and simulates (but does not submit) a contract call — used for the
 * read-only get_status / get_escrow views, which need no signature.
 */
async function readOnlyCall(method, args = []) {
  const server = getServer();
  const contractId = requireContractId();
  const contract = new StellarSdk.Contract(contractId);

  // Simulation only needs a valid source account, not necessarily the
  // connected wallet, but using it when available keeps things consistent.
  const sourcePublicKey = state.address || StellarSdk.Keypair.random().publicKey();
  const account = await server
    .getAccount(sourcePublicKey)
    .catch(() => new StellarSdk.Account(sourcePublicKey, "0"));

  const tx = new StellarSdk.TransactionBuilder(account, {
    fee: StellarSdk.BASE_FEE,
    networkPassphrase: NETWORKS[state.network].passphrase,
  })
    .addOperation(contract.call(method, ...args))
    .setTimeout(30)
    .build();

  const sim = await server.simulateTransaction(tx);
  if (StellarSdk.rpc?.Api?.isSimulationError?.(sim) || sim.error) {
    throw new Error(sim.error || "Simulation failed");
  }
  return StellarSdk.scValToNative(sim.result.retval);
}

/**
 * Builds, simulates, signs (via Freighter), and submits a state-changing
 * contract call. Polls until the transaction reaches a final status.
 */
async function invokeContract(method, args = []) {
  const server = getServer();
  const contractId = requireContractId();
  const walletAddress = requireWallet();
  const contract = new StellarSdk.Contract(contractId);

  const account = await server.getAccount(walletAddress);
  const tx = new StellarSdk.TransactionBuilder(account, {
    fee: StellarSdk.BASE_FEE,
    networkPassphrase: NETWORKS[state.network].passphrase,
  })
    .addOperation(contract.call(method, ...args))
    .setTimeout(60)
    .build();

  const prepared = await server.prepareTransaction(tx);

  const signResult = await Freighter.signTransaction(prepared.toXDR(), {
    networkPassphrase: NETWORKS[state.network].passphrase,
    address: walletAddress,
  });
  if (signResult.error) throw new Error(freighterErrorMessage(signResult.error));

  const signedTx = StellarSdk.TransactionBuilder.fromXDR(
    signResult.signedTxXdr,
    NETWORKS[state.network].passphrase
  );
  const sendResponse = await server.sendTransaction(signedTx);

  if (sendResponse.status === "ERROR") {
    throw new Error(`Submission failed: ${JSON.stringify(sendResponse.errorResult)}`);
  }

  const hash = sendResponse.hash;
  let result = await server.getTransaction(hash);
  const start = Date.now();
  while (result.status === "NOT_FOUND" && Date.now() - start < 30000) {
    await new Promise((r) => setTimeout(r, 1500));
    result = await server.getTransaction(hash);
  }

  if (result.status !== "SUCCESS") {
    throw Object.assign(new Error(`Transaction ${result.status}`), { hash });
  }

  return { hash, result };
}

async function runAction(label, fn) {
  try {
    const { hash } = await fn();
    logEntry({ title: `${label} succeeded`, hash });
    await refreshStatus();
  } catch (err) {
    logEntry({
      title: `${label} failed`,
      detail: err.message || String(err),
      hash: err.hash,
      isError: true,
    });
  }
}

// ---------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------

async function doInitialize() {
  await runAction("Initialize", async () => {
    const payer = requireWallet();
    const payee = initPayeeInput.value.trim();
    const arbiter = initArbiterInput.value.trim();
    const token = initTokenInput.value.trim();
    const amount = initAmountInput.value.trim();
    if (!payee || !arbiter || !token || !amount) {
      throw new Error("Fill in payee, arbiter, token, and amount.");
    }
    return invokeContract("initialize", [
      addressArg(payer),
      addressArg(payee),
      addressArg(arbiter),
      addressArg(token),
      i128Arg(amount),
    ]);
  });
}

async function doDeposit() {
  await runAction("Deposit", () => invokeContract("deposit", []));
}

async function doRelease() {
  await runAction("Release", () => {
    const caller = requireWallet();
    return invokeContract("release", [addressArg(caller)]);
  });
}

async function doRefund() {
  await runAction("Refund", () => invokeContract("refund", []));
}

function renderStatus(statusValue) {
  const label = STATUS_LABELS[statusValue] ?? "Unknown";
  statusBadge.textContent = label;
  statusBadge.className = `status-badge ${label.toLowerCase()}`;
}

async function refreshStatus() {
  try {
    const status = await readOnlyCall("get_status");
    renderStatus(typeof status === "object" && status?.tag ? STATUS_LABELS.indexOf(status.tag) : status);

    const escrow = await readOnlyCall("get_escrow");
    el("d-payer").textContent = escrow.payer;
    el("d-payee").textContent = escrow.payee;
    el("d-arbiter").textContent = escrow.arbiter;
    el("d-token").textContent = escrow.token;
    el("d-amount").textContent = escrow.amount.toString();
    el("d-funded").textContent = escrow.funded ? "Yes" : "No";
    detailsList.hidden = false;
  } catch (err) {
    logEntry({ title: "Could not load escrow status", detail: err.message || String(err), isError: true });
  }
}

// ---------------------------------------------------------------------
// Wire up event listeners
// ---------------------------------------------------------------------

connectBtn.addEventListener("click", connectWallet);
saveSettingsBtn.addEventListener("click", saveSettings);
initBtn.addEventListener("click", doInitialize);
depositBtn.addEventListener("click", doDeposit);
releaseBtn.addEventListener("click", doRelease);
refundBtn.addEventListener("click", doRefund);
refreshBtn.addEventListener("click", refreshStatus);

renderWallet();
