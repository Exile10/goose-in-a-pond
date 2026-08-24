#!/usr/bin/env node
/**
 * A virtual Matter device, for testing GIAP's Matter support without hardware.
 *
 *   cd matter-server && npm run virtual-device          # a dimmable light
 *   cd matter-server && npm run virtual-device -- lock  # a door lock
 *
 * It advertises one device for commissioning and prints its pairing code. Pair it
 * from the Devices tab, then drive it from chat — "turn off the light" — and
 * watch this process report what it was told to do.
 *
 * One device per run, deliberately. A light and a lock on one node is a composed
 * device that GIAP types by its first application endpoint, so the lock's controls
 * would be described as things a light can do — which is a worse test than either
 * device alone, and not a shape any real product ships.
 *
 * This exists because the alternative was Google's Matter Virtual Device app,
 * which is a separate download, is not scriptable, and cannot be run in CI. Here
 * the device is the same matter.js the controller already depends on, so testing
 * the Matter path needs nothing that is not already in the repo.
 *
 * A DEV TOOL. It is not shipped: `server_setup.rs` copies `src/`, and this is
 * not in it. It writes its fabric to a temp directory that is cleared on every
 * start, so each run is a fresh, uncommissioned device.
 */

import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  ClusterBehavior,
  ClusterId,
  Endpoint,
  Environment,
  LogFormat,
  Logger,
  Minutes,
  ServerNode,
  Time,
} from "@matter/main";
import { DoorLockServer } from "@matter/main/behaviors/door-lock";
import { DimmableLightDevice } from "@matter/main/devices/dimmable-light";
import { DoorLockDevice } from "@matter/main/devices/door-lock";
import { ClusterType, TlvBoolean, TlvString, WritableAttribute } from "@matter/main/types";

// ── The light ────────────────────────────────────────────────────────────────

/**
 * A manufacturer-specific cluster, so the one thing GIAP cannot name is reproducible
 * without a separate download.
 *
 * Modelled on the Custom Clusters panel of Google's Matter Virtual Device, which shows
 * a Flip-Flop toggle and an Emoticon field. The names below are for this file's reader
 * only: a manufacturer cluster publishes no attribute names, so a controller sees
 * cluster 0xfff1fc01 and nothing inside it. That is exactly the case `vendor_clusters`
 * exists to disclose.
 *
 * 0xfff1 is the test vendor id this device already uses; 0xfc01 is in Matter's
 * manufacturer-specific cluster range.
 */
const GiapCustomCluster = ClusterType({
  id: ClusterId(0xfff1fc01),
  name: "GiapCustom",
  revision: 1,
  attributes: {
    flipFlop: WritableAttribute(0x0, TlvBoolean, { default: false }),
    emoticon: WritableAttribute(0x1, TlvString, { default: "smile" }),
  },
});

class GiapCustomServer extends ClusterBehavior.for(GiapCustomCluster) {
  // Stated as a literal so the composed device type below keeps its named behaviors.
  // Derived from the cluster name it is a widened `Uncapitalize<string>`, which turns
  // the endpoint's options into an index signature that swallows `id`.
  static override readonly id = "giapCustom" as const;
}

/** A dimmable light that also carries a cluster GIAP cannot name. */
const CustomLightDevice = DimmableLightDevice.with(GiapCustomServer);

// ── The lock ─────────────────────────────────────────────────────────────────

/**
 * A door lock with the two optional features whose attributes GIAP reports but cannot
 * set: the door position sensor, and the PIN requirement for remote operation.
 *
 * `CredentialOverTheAirAccess` is not optional decoration here. Matter conforms
 * `requirePinForRemoteOperation` on **COTA and PIN together**, so matter.js refuses the
 * attribute outright when only PinCredential is declared — a lock built without it
 * looks like one that simply has no PIN requirement, which is the case this device
 * exists to reproduce.
 */
const FeaturedLock = DoorLockServer.with(
  "DoorPositionSensor",
  "PinCredential",
  "CredentialOverTheAirAccess",
  "User",
);
const FeaturedLockDevice = DoorLockDevice.with(FeaturedLock);

/** `doorState` by the word this tool takes for it, matching what GIAP reports. */
const DOOR_STATES = {
  open: 0,
  closed: 1,
  jammed: 2,
  forced: 3,
  ajar: 5,
} as const;

type DoorWord = keyof typeof DOOR_STATES;

function isDoorWord(word: string): word is DoorWord {
  return word in DOOR_STATES;
}

/**
 * A lock's opening state, as Matter insists it be stated.
 *
 * Every value here is mandatory for the declared features, and two are not guessable:
 * `operatingMode` must be set even though a lock has an obvious default, and
 * `supportedOperatingModes` has **inverted bits** — a set bit means *not* supported —
 * with a reserved range that must be all ones. matter.js rejects the device rather
 * than the attribute if either is wrong, so getting it wrong reads as a broken tool.
 */
const LOCK_STATE = {
  lockState: 1, // Locked
  lockType: 0, // deadbolt
  actuatorEnabled: true,
  doorState: DOOR_STATES.open, // starts disagreeing with the bolt, on purpose
  numberOfPinUsersSupported: 5,
  maxPinCodeLength: 8,
  minPinCodeLength: 4,
  wrongCodeEntryLimit: 3,
  userCodeTemporaryDisableTime: 10,
  operatingMode: 0, // Normal
  supportedOperatingModes: {
    normal: false,
    vacation: true,
    privacy: true,
    noRemoteLockUnlock: true,
    passage: true,
    alwaysSet: 0x7ff,
  },
  requirePinForRemoteOperation: false,
};

// ── Which device to be ───────────────────────────────────────────────────────

type Kind = "light" | "lock";

interface Model {
  /** How the device is named to a person and on the fabric. */
  label: string;
  productName: string;
  productId: number;
  serial: string;
  /** What to try in chat once it is paired. */
  tryThis: string[];
}

const MODELS: Record<Kind, Model> = {
  light: {
    label: "Test Light (dimmable)",
    productName: "Test Light",
    productId: 0x8001,
    serial: "giap-virtual-0001",
    tryThis: [
      `"turn off the light"`,
      `"set the light to 40%"`,
      `"what can the light do?"  -- should name a manufacturer-specific`,
      `                             cluster it cannot drive`,
    ],
  },
  lock: {
    label: "Test Lock (door position + PIN)",
    productName: "Test Lock",
    productId: 0x8004,
    serial: "giap-virtual-0002",
    tryThis: [
      `"lock the door"`,
      `"is the door closed?"  -- the bolt starts thrown with the door`,
      `                          standing open, which is the whole point`,
      `"what can the lock do?"  -- should report a door state and a PIN`,
      `                            requirement it cannot set`,
    ],
  },
};

function kindFromArgv(argv: readonly string[]): Kind | undefined {
  // Nothing named means the light, which is what this tool was before it had a choice.
  const named = argv.find(arg => !arg.startsWith("-"));
  if (named === undefined) return "light";
  return named in MODELS ? (named as Kind) : undefined;
}

/** Cleared on every start: a half-commissioned device from a previous run is the
 *  most confusing thing this tool could hand you, and switching kind makes one. */
const STORAGE = join(tmpdir(), "giap-virtual-device");

/** Fixed so the pairing code is the same on every run, which makes it something
 *  you can keep in a scratch file rather than re-copy each time. */
const PASSCODE = 20202021;
const DISCRIMINATOR = 3840;

async function main(): Promise<void> {
  const kind = kindFromArgv(process.argv.slice(2));
  if (kind === undefined) {
    console.error(
      `\n  Unknown device. Pass one of: ${Object.keys(MODELS).join(", ")}\n\n` +
        `    npm run virtual-device\n` +
        `    npm run virtual-device -- lock\n`,
    );
    process.exit(2);
  }
  const model = MODELS[kind];

  rmSync(STORAGE, { recursive: true, force: true });
  Logger.format = LogFormat.PLAIN;
  Logger.level = "info";

  Environment.default.vars.set("storage.path", STORAGE);

  const node = await ServerNode.create({
    id: "giap-virtual-device",
    // 5541, not Matter's standard 5540, so this can run alongside a real device
    // app that wants the standard port — Google's Matter Virtual Device among
    // them. The controller used to squat 5540 itself, which is what broke those
    // apps; it no longer does, so 5540 is left for whoever actually needs it.
    network: { port: 5541 },
    commissioning: { passcode: PASSCODE, discriminator: DISCRIMINATOR },
    productDescription: {
      name: `GIAP ${model.productName}`,
      deviceType:
        kind === "light" ? DimmableLightDevice.deviceType : DoorLockDevice.deviceType,
    },
    basicInformation: {
      vendorName: "GIAP",
      vendorId: 0xfff1 as never,
      productName: model.productName,
      productId: model.productId,
      nodeLabel: model.productName,
      serialNumber: model.serial,
    },
  });

  let commands = "";
  if (kind === "light") {
    // On/Off gives `power`, Level Control gives `brightness`, so both of the verbs a
    // user is most likely to try are covered. The custom cluster rides along so the
    // "has a control GIAP cannot drive" path has a device to run against.
    reportLight(await node.add(CustomLightDevice, { id: "light" }));
  } else {
    const lock = await node.add(FeaturedLockDevice, { id: "lock", doorLock: LOCK_STATE });
    reportLock(lock);
    commands = watchStdin(lock);
  }

  await node.start();

  const { qrPairingCode, manualPairingCode } = node.state.commissioning.pairingCodes;
  const paired = node.lifecycle.isCommissioned;

  console.log(`
────────────────────────────────────────────────────────────
  GIAP virtual Matter device — "${model.label}"
────────────────────────────────────────────────────────────

  ${paired ? "Already commissioned." : "Waiting to be paired."}

  Pair it from the Devices tab with EITHER of these:

    manual pairing code   ${manualPairingCode}
    QR payload            ${qrPairingCode}

  Then try, in chat:   ${model.tryThis.join("\n                       ")}
${commands}
  Ctrl-C to stop. Its fabric is temporary, so stopping and
  starting gives you a fresh, unpaired device.
────────────────────────────────────────────────────────────
`);

  // The Matter commissioning window is ~15 minutes from boot, and "it was
  // pairable earlier" is the most common way pairing fails. Say so rather than
  // leaving the user to discover it.
  Time.getTimer("pairing window", Minutes(15), () => {
    if (!node.lifecycle.isCommissioned) {
      console.log(
        "\n  The 15-minute commissioning window has closed. Restart this tool to reopen it.\n",
      );
    }
  }).start();
}

/** Print what the device is told to do, so a command that reached it is visible
 *  from this side as well as GIAP's. */
function reportLight(light: Endpoint<typeof CustomLightDevice>): void {
  light.events.onOff.onOff$Changed.on(value => {
    console.log(`  [device] power  -> ${value ? "on" : "off"}`);
  });
  light.events.levelControl.currentLevel$Changed.on((value: number | null) => {
    // Matter's level scale is 0-254; GIAP speaks percent, so show both or the
    // number looks wrong next to what was asked for.
    const percent = value === null ? "unknown" : `${Math.round((value * 100) / 254)}%`;
    console.log(`  [device] level  -> ${value} (${percent})`);
  });
}

function reportLock(lock: Endpoint<typeof FeaturedLockDevice>): void {
  lock.events.doorLock.lockState$Changed.on(value => {
    console.log(`  [device] bolt   -> ${value === 1 ? "locked" : "unlocked"} (${value})`);
  });
}

/**
 * Let the operator move the door, which GIAP cannot.
 *
 * The point of the door state is that nothing on GIAP's side writes it — so without a
 * way to change it here, the only reading it can ever be asked about is the one it
 * started on. This is the stand-in for the dropdown Google's app has.
 */
function watchStdin(lock: Endpoint<typeof FeaturedLockDevice>): string {
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", chunk => {
    for (const line of chunk.toString().split("\n")) {
      const word = line.trim().toLowerCase();
      if (word === "") continue;

      if (isDoorWord(word)) {
        void lock
          .setStateOf(FeaturedLock, { doorState: DOOR_STATES[word] })
          .then(() => console.log(`  [device] door   -> ${word}`))
          .catch((error: unknown) => console.log(`  [device] door refused: ${String(error)}`));
        continue;
      }
      if (word === "pin on" || word === "pin off") {
        void lock
          .setStateOf(FeaturedLock, { requirePinForRemoteOperation: word === "pin on" })
          .then(() => console.log(`  [device] pin    -> ${word === "pin on" ? "required" : "not required"}`))
          .catch((error: unknown) => console.log(`  [device] pin refused: ${String(error)}`));
        continue;
      }
      console.log(`  [device] unknown: "${word}" — try ${Object.keys(DOOR_STATES).join(", ")}, pin on, pin off`);
    }
  });

  return `
  Type here to move the door, since nothing on GIAP's side can:

    ${Object.keys(DOOR_STATES).join(" / ")}
    pin on / pin off
`;
}

main().catch((error: unknown) => {
  console.error("virtual device failed:", error);
  process.exit(1);
});
