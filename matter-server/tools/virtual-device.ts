#!/usr/bin/env node
/**
 * A virtual Matter device, for testing GIAP's Matter support without hardware.
 *
 *   cd matter-server && npm run virtual-device
 *
 * It advertises itself for commissioning and prints its pairing code. Pair it
 * from the Devices tab, then drive it from chat — "turn off the light" — and
 * watch this process report what it was told to do.
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

import { Endpoint, Environment, LogFormat, Logger, Minutes, ServerNode, Time } from "@matter/main";
import { DimmableLightDevice } from "@matter/main/devices/dimmable-light";

/** Cleared on every start: a half-commissioned device from a previous run is the
 *  most confusing thing this tool could hand you. */
const STORAGE = join(tmpdir(), "giap-virtual-device");

/** Fixed so the pairing code is the same on every run, which makes it something
 *  you can keep in a scratch file rather than re-copy each time. */
const PASSCODE = 20202021;
const DISCRIMINATOR = 3840;

async function main(): Promise<void> {
  rmSync(STORAGE, { recursive: true, force: true });
  Logger.format = LogFormat.PLAIN;
  Logger.level = "info";

  Environment.default.vars.set("storage.path", STORAGE);

  const node = await ServerNode.create({
    id: "giap-virtual-device",
    network: { port: 5541 },
    commissioning: { passcode: PASSCODE, discriminator: DISCRIMINATOR },
    productDescription: { name: "GIAP Test Light", deviceType: DimmableLightDevice.deviceType },
    basicInformation: {
      vendorName: "GIAP",
      vendorId: 0xfff1 as never,
      productName: "Test Light",
      productId: 0x8001,
      nodeLabel: "Test Light",
      serialNumber: "giap-virtual-0001",
    },
  });

  // A dimmable light: On/Off gives `power`, Level Control gives `brightness`, so
  // both of the verbs a user is most likely to try are covered.
  const light = await node.add(DimmableLightDevice, { id: "light" });
  reportChanges(light);

  await node.start();

  const { qrPairingCode, manualPairingCode } = node.state.commissioning.pairingCodes;
  const paired = node.lifecycle.isCommissioned;

  console.log(`
────────────────────────────────────────────────────────────
  GIAP virtual Matter device — "Test Light" (dimmable)
────────────────────────────────────────────────────────────

  ${paired ? "Already commissioned." : "Waiting to be paired."}

  Pair it from the Devices tab with EITHER of these:

    manual pairing code   ${manualPairingCode}
    QR payload            ${qrPairingCode}

  Then try, in chat:   "turn off the light"
                       "set the light to 40%"

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
function reportChanges(light: Endpoint<typeof DimmableLightDevice>): void {
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

main().catch((error: unknown) => {
  console.error("virtual device failed:", error);
  process.exit(1);
});
