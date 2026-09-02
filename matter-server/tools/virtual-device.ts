/**
 * A Matter device, for testing GIAP against.
 *
 * Not part of the controller. This is the device side — the thing GIAP commissions —
 * and it exists because `docs/matter.md` was right that there was no device-side test
 * rig in this repo, and because the two cases that matter most cannot be built any
 * other way:
 *
 *   - A BRIDGE. One Matter node carrying an Aggregator whose children are separate
 *     logical devices. Google's Matter Virtual Device cannot express one, and no
 *     recorded fixture can either, because a fixture never crosses the socket.
 *   - A device type nobody owns. Ninety percent of the Matter Device Library is
 *     hardware this project will never have on a desk.
 *
 * It needs no new dependency: matter.js is already here for the controller, and it
 * ships the whole device library — 81 device types plus the Aggregator and Bridged
 * Node endpoints. The same library on both sides means this tests GIAP's mappings
 * rather than matter.js's conformance, which is the right target: the mappings are
 * what is in doubt. Where MVD can express the same device, prefer MVD — it is
 * Google's own stack, so it is the independent check.
 *
 * Usage:
 *
 *   node --import tsx tools/virtual-device.ts --device dimmable-light
 *   node --import tsx tools/virtual-device.ts --device oven --part temperature-controlled-cabinet
 *   node --import tsx tools/virtual-device.ts \
 *     --bridged dimmable-light=Kitchen --bridged dimmable-light=Hall --bridged door-lock=Front
 *
 * Then paste the printed code into GIAP's Register-device dialog.
 *
 * Once running, stdin takes commands so a subscription report can be provoked rather
 * than waited for:
 *
 *   set kitchen.onOff.onOff = true
 *   list
 *   quit
 */

import {
  DeviceTypeId,
  Environment,
  ServerNode,
  VendorId,
  type Endpoint,
  type EndpointType,
} from "@matter/main";
import { AggregatorEndpoint } from "@matter/main/endpoints/aggregator";
import { BridgedDeviceBasicInformationServer } from "@matter/main/behaviors/bridged-device-basic-information";

import { parseArgs, type Spec, type DeviceSpec } from "./virtual-device-args.js";

/** Matter's Bridged Node device type. A child claiming this IS a separate device. */
const BRIDGED_NODE_DEVICE_TYPE = 19;

/**
 * A device type by its module name, e.g. `dimmable-light` -> `DimmableLightDevice`.
 *
 * Dynamically imported so the tool carries no table of 81 names. The convention is
 * exactly regular across the library — every `devices/<kebab>.js` exports
 * `${PascalCase}Device` — and matter.js's own model round-trips it: for all 81,
 * `Matter.deviceTypes(id).name + "Device"` is the export name.
 *
 * `@matter/main/...` rather than `@matter/node/...` on purpose. Only the `main`
 * subpaths pull in `@matter/main/platform`, which installs the Node.js environment;
 * and `@matter/node` is a transitive dependency, so importing it directly is a bet
 * on hoisting.
 */
async function deviceTypeNamed(name: string): Promise<DeviceType> {
  const exportName = `${name.replace(/(^|-)([a-z])/g, (_, __, c: string) => c.toUpperCase())}Device`;
  let module: Record<string, unknown>;
  try {
    module = (await import(`@matter/main/devices/${name}`)) as Record<string, unknown>;
  } catch {
    throw new Error(
      `no such device type '${name}'. The name is the matter.js module name, ` +
        `kebab-cased: dimmable-light, room-air-conditioner, temperature-controlled-cabinet.`,
    );
  }
  const type = module[exportName];
  if (!isDeviceType(type)) {
    throw new Error(`'${name}' resolved to no ${exportName} export`);
  }
  return type;
}

/**
 * A device type as this tool handles one.
 *
 * The dynamic import erases the precise type — matter.js's device constants are
 * `MutableEndpoint.With<...>` singletons and there is no way to name the one behind a
 * runtime string. So the boundary is typed once, here, with the three members the
 * tool actually uses, rather than sprinkling casts through the call sites.
 */
type DeviceType = EndpointType & {
  deviceRevision: number;
  with(...behaviors: unknown[]): DeviceType;
};

function isDeviceType(value: unknown): value is DeviceType {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { deviceType?: unknown }).deviceType === "number" &&
    "behaviors" in value
  );
}

/**
 * Add one bridged child under the aggregator.
 *
 * Two things here are not obvious and both are load-bearing.
 *
 * `BridgedDeviceBasicInformationServer` has to be composed onto the DEVICE type,
 * because that cluster is what carries a bridged child's own name and its own
 * reachability — without it every child on a hub is nameless and the hub's liveness
 * is the only liveness there is.
 *
 * And `descriptor.deviceTypeList` has to be seeded by hand. `DescriptorServer`
 * synthesises the list only when it is empty, and it synthesises just the device's
 * own type — so a bridged light advertises `[{257}]` and nothing says Bridged Node.
 * A controller detects a bridged child by exactly that missing 19, so without this
 * seed the whole bridge silently looks like an ordinary composed device, and the
 * case under test is not the case being run.
 */
async function addBridgedChild(
  aggregator: Endpoint,
  spec: DeviceSpec,
): Promise<Endpoint> {
  const type = await deviceTypeNamed(spec.device);
  const bridged = type.with(BridgedDeviceBasicInformationServer);

  return aggregator.add(bridged, {
    id: spec.id,
    ...(spec.number === undefined ? {} : { number: spec.number }),
    ...spec.state,
    bridgedDeviceBasicInformation: {
      nodeLabel: spec.label ?? spec.id,
      productName: spec.device,
      vendorName: "GIAP virtual device",
      reachable: true,
    },
    descriptor: {
      deviceTypeList: [
        { deviceType: DeviceTypeId(BRIDGED_NODE_DEVICE_TYPE), revision: 3 },
        { deviceType: type.deviceType, revision: type.deviceRevision },
      ],
    },
  });
}

/** Add a plain child endpoint — a part of a composed device, not a bridged one. */
async function addPart(parent: Endpoint, spec: DeviceSpec): Promise<Endpoint> {
  const type = await deviceTypeNamed(spec.device);
  return parent.add(type, {
    id: spec.id,
    ...(spec.number === undefined ? {} : { number: spec.number }),
    ...spec.state,
  });
}

async function build(spec: Spec): Promise<{ node: ServerNode; endpoints: Map<string, Endpoint> }> {
  Environment.default.vars.set("storage.path", spec.storage);

  const primary = spec.device === undefined ? undefined : await deviceTypeNamed(spec.device.device);

  const node = await ServerNode.create({
    // The node id names its own storage directory under `storage.path`, so a
    // distinct id is all the isolation two concurrent instances need.
    id: spec.id,
    // NOT 5540. A controller or another device holding it stops every other Matter
    // device on the machine from starting, with nothing pointing back at the culprit
    // — see "Ports, and why a device app may refuse to start" in docs/matter.md.
    // MVD's port is editable too, so leave 5540 to it.
    network: { port: spec.port },
    commissioning: { passcode: spec.passcode, discriminator: spec.discriminator },
    productDescription: {
      name: spec.name,
      deviceType: (primary ?? AggregatorEndpoint).deviceType,
    },
    basicInformation: {
      vendorId: VendorId(spec.vendorId),
      vendorName: "GIAP virtual device",
      productId: spec.productId,
      productName: spec.name,
    },
  });

  const endpoints = new Map<string, Endpoint>();

  if (primary !== undefined && spec.device !== undefined) {
    const own = await node.add(primary, {
      id: spec.device.id,
      ...(spec.device.number === undefined ? {} : { number: spec.device.number }),
      ...spec.device.state,
    });
    endpoints.set(spec.device.id, own);
    for (const part of spec.parts) {
      endpoints.set(part.id, await addPart(own, part));
    }
  }

  if (spec.bridged.length > 0) {
    const aggregator = await node.add(AggregatorEndpoint, { id: "aggregator" });
    endpoints.set("aggregator", aggregator);
    for (const child of spec.bridged) {
      const endpoint = await addBridgedChild(aggregator, child);
      endpoints.set(child.id, endpoint);
      // A part of a bridged child, so a composed device behind a hub is expressible.
      for (const part of child.parts) {
        endpoints.set(part.id, await addPart(endpoint, part));
      }
    }
  }

  await node.start();
  return { node, endpoints };
}

/**
 * Apply `set <endpoint>.<behavior>.<attribute> = <value>`.
 *
 * One transaction per line, so a controller sees one coherent subscription report
 * rather than a torn read. The value is JSON when it parses as JSON — so `true`,
 * `42` and `null` mean what they look like — and a bare string otherwise.
 */
async function applySet(endpoints: Map<string, Endpoint>, line: string): Promise<string> {
  const match = /^set\s+([\w-]+)\.(\w+)\.(\w+)\s*=\s*(.+)$/.exec(line);
  if (match === null) {
    return "usage: set <endpoint>.<behavior>.<attribute> = <value>";
  }
  const [, endpointId, behavior, attribute, raw] = match;
  const endpoint = endpoints.get(endpointId!);
  if (endpoint === undefined) {
    return `no endpoint '${endpointId}'. Known: ${[...endpoints.keys()].join(", ")}`;
  }
  let value: unknown;
  try {
    value = JSON.parse(raw!);
  } catch {
    value = raw;
  }
  try {
    await endpoint.set({ [behavior!]: { [attribute!]: value } });
    return `${endpointId}.${behavior}.${attribute} = ${JSON.stringify(value)}`;
  } catch (error) {
    return `refused: ${error instanceof Error ? error.message : String(error)}`;
  }
}

/**
 * The innermost reason, without the stack.
 *
 * matter.js's conformance errors say exactly what is missing — "Validating
 * hub.aggregator.front.doorLock.state.lockType: Matter requires you to set this
 * attribute" — but they arrive nested three deep inside a construction failure, so
 * the useful sentence is forty lines below the useless one.
 */
function deepestCause(error: unknown): string {
  let current = error;
  const seen = new Set<unknown>();
  while (current instanceof Error && current.cause !== undefined && !seen.has(current)) {
    seen.add(current);
    current = current.cause;
  }
  if (current instanceof AggregateError && current.errors.length > 0) {
    return deepestCause(current.errors[0]);
  }
  return current instanceof Error ? current.message : String(current);
}

async function main(): Promise<void> {
  let spec: Spec;
  try {
    spec = parseArgs(process.argv.slice(2));
  } catch (error) {
    // A wrong flag is a typo, not a crash. Without this it arrived as a matter.js
    // FATAL and a stack trace, which buries the one sentence that helps.
    console.error(`\n  ${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(2);
  }

  let node: ServerNode;
  let endpoints: Map<string, Endpoint>;
  try {
    ({ node, endpoints } = await build(spec));
  } catch (error) {
    const reason = deepestCause(error);
    console.error(`\n  could not build the device: ${reason}\n`);
    if (/requires you to set this attribute|not within bounds defined by constraint/i.test(reason)) {
      // Deliberately not a table of mandatory defaults for 81 device types: matter.js
      // already knows which attribute, and one flag is a shorter path than a lookup
      // table that would drift from the spec.
      console.error("  Matter constrains that attribute on this device type, and matter.js");
      console.error("  enforces it device-side. Give it a value with --attr, e.g.");
      console.error("    --attr front.doorLock.lockType=0 --attr front.doorLock.wrongCodeEntryLimit=5\n");
    }
    process.exit(1);
  }

  const { manualPairingCode, qrPairingCode } = node.state.commissioning.pairingCodes;
  console.log("");
  console.log(`  ${spec.name} is up on Matter port ${spec.port}.`);
  console.log("");
  console.log(`  manual pairing code   ${manualPairingCode}`);
  console.log(`  QR payload            ${qrPairingCode}`);
  console.log("");
  console.log(`  endpoints             ${[...endpoints.keys()].join(", ") || "(none)"}`);
  console.log("");
  console.log("  Commands: set <endpoint>.<behavior>.<attribute> = <value> | list | quit");
  console.log("");

  // `close()`, never `stop()`. Only close reaches the storage layer and releases the
  // directory lock; stop leaves it held, and matter.js then warns on process exit
  // that it is removing an orphaned lock.
  let closing = false;
  const shutdown = async (): Promise<never> => {
    if (!closing) {
      closing = true;
      await node.close();
    }
    process.exit(0);
  };
  process.once("SIGINT", () => void shutdown());
  process.once("SIGTERM", () => void shutdown());

  process.stdin.setEncoding("utf8");
  for await (const chunk of process.stdin) {
    const lines = String(chunk)
      .split("\n")
      .map(line => line.trim())
      .filter(line => line.length > 0);
    for (const line of lines) {
      if (line === "quit" || line === "exit") {
        await shutdown();
      } else if (line === "list") {
        for (const [id, endpoint] of endpoints) {
          console.log(`  ${id}  endpoint ${endpoint.number}  ${endpoint.type.name}`);
        }
      } else {
        console.log(`  ${await applySet(endpoints, line)}`);
      }
    }
  }
  await shutdown();
}

await main();
