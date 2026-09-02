/**
 * Recorded devices, carried over from the Rust adapter's own fixtures.
 *
 * These are what the mappings were calibrated against — the Matter Virtual Device's
 * fan and light as commissioned on 2026-08-05, and the plug/bulb pair whose
 * indistinguishability is the reason device typing reads the Descriptor at all. They
 * moved here with the cluster logic so the coverage moved with it rather than being
 * lost in the port.
 */

import type {
  ClusterState,
  EndpointSnapshot,
  NodeSnapshot,
  VendorCluster,
} from "../src/mapping/snapshot.js";

export function endpoint(
  number: number,
  clusters: ClusterState,
  deviceTypes: number[] = [],
  vendorClusters: VendorCluster[] = [],
  parts: number[] = [],
): EndpointSnapshot {
  return { number, deviceTypes, clusters, vendorClusters, parts };
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

/**
 * Google's Matter Virtual Device 1.7.0 as it ships: a dimmable light, plus the custom
 * cluster its Controller tab shows as a Flip-Flop toggle and an Emoticon field.
 * Neither of those names appears anywhere on the wire, and neither does a count of
 * them, which is the whole reason this fixture is one bare id.
 */
export function customLightNode(): NodeSnapshot {
  return node(31, [
    named("Virtual Custom OnOff Light"),
    endpoint(1, { onOff: { onOff: false }, levelControl: { currentLevel: 0 } }, [0x0101], [
      { id: 0xfff1fc01 },
    ]),
  ]);
}

/**
 * Google's Virtual Door Lock as its Controller tab shows it: a door state, a lock
 * state, and a PIN requirement for remote operation. Both DoorPositionSensor and
 * PinCredential are optional DoorLock features, so a lock without them has neither
 * attribute — which is why the bare-lock fixture below exists beside this one.
 */
export function doorLockNode(): NodeSnapshot {
  return node(44, [
    named("Virtual Door Lock"),
    endpoint(1, {
      doorLock: { lockState: 1, doorState: 0, requirePinForRemoteOperation: false },
    }, [0x000a]),
  ]);
}

/** A lock with neither optional feature: lockState and nothing else. */
export function bareLockNode(): NodeSnapshot {
  return node(45, [named("Deadbolt"), endpoint(1, { doorLock: { lockState: 1 } }, [0x000a])]);
}

/**
 * Google's Virtual Extended Color Light: hue/saturation, XY and colour temperature, the
 * three modes its Color mode dropdown offers. `colorCapabilities` bit 0 is
 * HueSaturation, bit 3 Xy, bit 4 ColorTemperature.
 */
export function extendedColorLightNode(): NodeSnapshot {
  return node(51, [
    named("Virtual Extended Color Light"),
    endpoint(1, {
      onOff: { onOff: true },
      levelControl: { currentLevel: 254 },
      colorControl: {
        colorCapabilities: 0x19,
        colorMode: 0,
        currentHue: 0,
        currentSaturation: 0,
        colorTemperatureMireds: 250,
        colorTempPhysicalMinMireds: 153,
        colorTempPhysicalMaxMireds: 500,
      },
    }, [0x010d]),
  ]);
}

/**
 * Google's Extended Color Light as it actually reports itself: every colour mode
 * offered in its Controller tab, and a `colorCapabilities` bitmap claiming none of
 * them. Non-conformant, and shipped — describing this device as having no colour is
 * the regression the claims-then-evidence order exists to prevent.
 */
export function mvdColorLightNode(): NodeSnapshot {
  return node(56, [
    named("Virtual Extended Color Light"),
    endpoint(1, {
      onOff: { onOff: true },
      levelControl: { currentLevel: 254 },
      colorControl: {
        colorCapabilities: {
          hueSaturation: false,
          enhancedHue: false,
          colorLoop: false,
          xy: false,
          colorTemperature: false,
        },
        colorMode: 0,
        currentHue: 0,
        currentSaturation: 0,
        currentX: 24939,
        currentY: 24701,
        colorTemperatureMireds: 250,
      },
    }, [0x010d]),
  ]);
}

/**
 * A tunable-white bulb: ColorControl with colour temperature and NO hue at all
 * (`colorCapabilities` bit 4 only). Very common, and the reason colour is gated on what
 * the device claims — offered a hue, this device rejects it.
 */
export function tunableWhiteNode(): NodeSnapshot {
  return node(52, [
    named("Reading Lamp"),
    endpoint(1, {
      onOff: { onOff: true },
      colorControl: {
        colorCapabilities: 0x10,
        colorMode: 2,
        colorTemperatureMireds: 370,
        colorTempPhysicalMinMireds: 200,
        colorTempPhysicalMaxMireds: 454,
      },
    }, [0x010c]),
  ]);
}

/**
 * A Room Air Conditioner as one really reports itself: cooling only, stated through the
 * mandatory `controlSequenceOfOperation`, with a cooling setpoint and NO heating one —
 * verified against a live commissioned device. `systemMode` arrives as the enum NAME
 * here, which is the encoding that made every numeric comparison miss.
 */
export function airConditionerNode(): NodeSnapshot {
  return node(61, [
    named("Room Air Conditioner"),
    endpoint(1, {
      onOff: { onOff: false },
      thermostat: {
        controlSequenceOfOperation: 0,
        systemMode: "Cool",
        localTemperature: 2500,
        occupiedCoolingSetpoint: 2400,
        absMinCoolSetpointLimit: 1600,
        absMaxCoolSetpointLimit: 3200,
        absMinHeatSetpointLimit: 700,
        absMaxHeatSetpointLimit: 3000,
      },
    }, [0x0072]),
  ]);
}

/**
 * A Smoke CO Alarm as one really reports itself, verified against a live commissioned
 * device: a mandatory `expressedState` summary, separate smoke and CO readings, and the
 * three health attributes that say whether it can still sound at all.
 *
 * Set the way the reported device was: sounding for CARBON MONOXIDE while its smoke
 * reading also sits at Critical. Only the smoke reading was mapped, so the one attribute
 * naming which danger it is was the one GIAP could not see.
 */
export function smokeCoAlarmNode(): NodeSnapshot {
  return node(71, [
    named("Smoke CO Alarm"),
    endpoint(1, {
      smokeCoAlarm: {
        featureMap: { smokeAlarm: true, coAlarm: true },
        expressedState: 2,
        smokeState: 2,
        coState: 2,
        batteryAlert: 0,
        endOfServiceAlert: 0,
        hardwareFaultAlert: false,
      },
    }, [0x0076]),
  ]);
}

/**
 * Google's Matter Virtual Device Generic Switch: a latching switch whose whole screen
 * is a "Current position" dropdown reading "Position #1".
 *
 * The device that arrived typed `matter` with no capabilities and answered "cannot be
 * controlled, and does not measure any data" -- because 0x000f was not a device type
 * GIAP mapped, `switch` was not a cluster the snapshot admitted, and nothing described
 * it. Two positions, so a bound of 0..1 is a real statement rather than the spec's
 * default repeated back.
 */
export function genericSwitchNode(): NodeSnapshot {
  return node(6, [
    named("Generic Switch"),
    endpoint(1, {
      switch: {
        featureMap: { latchingSwitch: true, momentarySwitch: false },
        numberOfPositions: 2,
        currentPosition: 1,
      },
    }, [0x000f]),
  ]);
}

/**
 * A momentary switch -- a pushbutton -- which says nothing about how many positions
 * it has.
 *
 * Two things this pins. No `numberOfPositions`, so no bound is stated rather than the
 * spec's default of 2 being invented for it. And `momentarySwitch`, so a reader is
 * told the position is fleeting: the presses themselves are Matter events, which the
 * controller does not subscribe to.
 */
export function momentarySwitchNode(): NodeSnapshot {
  return node(7, [
    named("Button"),
    endpoint(1, {
      switch: {
        featureMap: { latchingSwitch: false, momentarySwitch: true },
        currentPosition: 0,
      },
    }, [0x000f]),
  ]);
}

/** A CO-only alarm: no smoke feature, so no smoke reading exists at all. */
export function coOnlyAlarmNode(): NodeSnapshot {
  return node(72, [
    named("CO Alarm"),
    endpoint(1, {
      smokeCoAlarm: {
        // The feature map is what says the smoke sensor is absent. Value presence
        // cannot: an unsupported attribute and one that has not reported yet are both
        // undefined.
        featureMap: { smokeAlarm: false, coAlarm: true },
        expressedState: 0,
        coState: 0,
        batteryAlert: 0,
      },
    }, [0x0076]),
  ]);
}

/**
 * A Basic Video Player as one really reports itself, verified against a live commissioned
 * device: the player on endpoint 1 (type 0x28) and a SPEAKER on endpoint 2 (type 0x22),
 * which is where Level Control lives. Searching the node for that cluster found the
 * speaker's level and called it brightness.
 */
export function videoPlayerNode(): NodeSnapshot {
  return node(81, [
    named("Basic Video Player"),
    endpoint(1, {
      onOff: { onOff: true },
      mediaPlayback: { currentState: 0 },
      mediaInput: {
        currentInput: 1,
        inputList: [
          { index: 1, inputType: 4, name: "HDMI 1" },
          { index: 2, inputType: 4, name: "HDMI 2" },
        ],
      },
      audioOutput: {
        currentOutput: 1,
        outputList: [
          { index: 1, outputType: 0, name: "TV Speaker" },
          { index: 2, outputType: 3, name: "Soundbar" },
        ],
      },
    }, [0x0028]),
    endpoint(2, { onOff: { onOff: true }, levelControl: { currentLevel: 127 } }, [0x0022]),
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
        operationalState: {
          operationalState: 0,
          // A real washer publishes the states it has; the spec ties those to the
          // commands it accepts.
          operationalStateList: [
            { operationalStateId: 0, operationalStateLabel: "Stopped" },
            { operationalStateId: 1, operationalStateLabel: "Running" },
            { operationalStateId: 2, operationalStateLabel: "Paused" },
            { operationalStateId: 3, operationalStateLabel: "Error" },
          ],
        },
      },
      [0x0073],
    ),
  ]);
}
