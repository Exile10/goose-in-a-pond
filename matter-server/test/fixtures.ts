/**
 * Recorded devices, carried over from the Rust adapter's own fixtures.
 *
 * These are what the mappings were calibrated against — the Matter Virtual Device's
 * fan and light as commissioned on 2026-08-05, and the plug/bulb pair whose
 * indistinguishability is the reason device typing reads the Descriptor at all. They
 * moved here with the cluster logic so the coverage moved with it rather than being
 * lost in the port.
 */

import type { ClusterState, EndpointSnapshot, NodeSnapshot } from "../src/mapping/snapshot.js";

export function endpoint(
  number: number,
  clusters: ClusterState,
  deviceTypes: number[] = [],
): EndpointSnapshot {
  return { number, deviceTypes, clusters };
}

export function node(nodeId: number, endpoints: EndpointSnapshot[], online = true): NodeSnapshot {
  return { nodeId: BigInt(nodeId), online, endpoints };
}

/** The root endpoint carrying a user-assigned name, as every real device has. */
export function named(name: string): EndpointSnapshot {
  return endpoint(0, { basicInformation: { nodeLabel: name } }, [0x0016]);
}

/**
 * The Matter Virtual Device's fan: Fan Control on endpoint 1 and NO On/Off cluster at
 * all, which is what left it with no capabilities and unreachable by "turn on the fan".
 */
export function fanNode(): NodeSnapshot {
  return node(18, [
    named("Living Room Fan"),
    endpoint(1, { fanControl: { fanMode: 0, percentSetting: 0 } }),
  ]);
}

/** OnOff + LevelControl on endpoint 13, as commissioned in the live session and
 *  identical for real bulbs. */
export function lightNode(): NodeSnapshot {
  return node(2, [
    endpoint(0, {}),
    endpoint(13, { onOff: { onOff: false }, levelControl: { currentLevel: 128 } }),
  ]);
}

/** A node that states its type the way every real one does: a Descriptor
 *  DeviceTypeList on the application endpoint. */
export function describedNode(
  nodeId: number,
  deviceType: number,
  clusters: ClusterState = {},
): NodeSnapshot {
  return node(nodeId, [
    endpoint(0, {}, [0x0016]),
    endpoint(1, clusters, [deviceType]),
  ]);
}

/**
 * The Matter Virtual Device's Laundry Washer: On/Off, a wash mode, a temperature
 * level, spin speed and rinse count, and an operational state. Four of its five
 * clusters had no verb, so `describe` could only offer power — which is what sent
 * "set the spin speed to high" back as "this device only turns on and off".
 */
export function laundryWasherNode(): NodeSnapshot {
  return node(50, [
    named("Virtual Laundry Washer"),
    endpoint(
      1,
      {
        onOff: { onOff: false },
        laundryWasherMode: {
          currentMode: 0,
          supportedModes: [
            { label: "Normal", mode: 0 },
            { label: "Heavy", mode: 1 },
            { label: "Delicate", mode: 2 },
            { label: "Whites", mode: 3 },
          ],
        },
        temperatureControl: {
          selectedTemperatureLevel: 0,
          supportedTemperatureLevels: ["Cold", "Warm", "Hot"],
        },
        laundryWasherControls: {
          spinSpeeds: ["Low", "Medium", "High"],
          spinSpeedCurrent: 0,
          numberOfRinses: 1,
          supportedRinses: ["None", "Normal", "Extra", "Max"],
        },
        operationalState: { operationalState: 0 },
      },
      [0x0073],
    ),
  ]);
}
