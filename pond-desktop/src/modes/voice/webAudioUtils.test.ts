import { describe, it, expect } from "vitest";
import { splitSpeechUnits, splitSentences, normalizeForSpeech } from "./webAudioUtils";

describe("splitSpeechUnits", () => {
  it("splits opening clause after minimum words when allowClauses is true", () => {
    const partial = "I would be glad to help with that, and here are the";
    const [units1, rem1] = splitSpeechUnits(partial, true);
    expect(units1).toEqual(["I would be glad to help with that,"]);
    expect(rem1).toBe("and here are the");

    const full = "I would be glad to help with that, and here are the details.";
    const [units2, rem2] = splitSpeechUnits(full, true);
    expect(units2).toEqual([
      "I would be glad to help with that,",
      "and here are the details."
    ]);
    expect(rem2).toBe("");
  });

  it("keeps short clauses with the sentence", () => {
    const text = "Sure, let's start the project.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["Sure, let's start the project."]);
    expect(rem).toBe("");
  });

  it("does not split on numeric commas", () => {
    const text = "There are 1,000 units ready.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["There are 1,000 units ready."]);
    expect(rem).toBe("");
  });

  it("does not split on numeric decimals or abbreviations", () => {
    const text = "Dr. Smith noted the temperature was 25.3 degrees Celsius.";
    const [units, rem] = splitSpeechUnits(text, true);
    expect(units).toEqual(["Dr. Smith noted the temperature was 25.3 degrees Celsius."]);
    expect(rem).toBe("");
  });

  it("does not split on streaming decimal trailing period or empty clause tail", () => {
    const partialDec = "The temperature is 25.";
    const [units1, rem1] = splitSpeechUnits(partialDec, true);
    expect(units1).toEqual([]);
    expect(rem1).toBe("The temperature is 25.");

    const partialClause = "I would be glad to help with that,";
    const [units2, rem2] = splitSpeechUnits(partialClause, true);
    expect(units2).toEqual([]);
    expect(rem2).toBe("I would be glad to help with that,");
  });

  it("splits on terminals and newlines", () => {
    const text = "First sentence! Second sentence? Third sentence.\nFourth line.";
    const [units, rem] = splitSpeechUnits(text, false);
    expect(units).toEqual([
      "First sentence!",
      "Second sentence?",
      "Third sentence.",
      "Fourth line."
    ]);
    expect(rem).toBe("");
  });
});

describe("normalizeForSpeech", () => {
  it("normalizes Kenyan currency prefix and suffix", () => {
    expect(normalizeForSpeech("dollar to KSH rate")).toBe("dollar to Kenyan shillings rate");
    expect(normalizeForSpeech("exchange is 129.83 KES")).toBe("exchange is 129.83 Kenyan shillings");
    expect(normalizeForSpeech("costs KSh 130 per unit")).toBe("costs 130 Kenyan shillings per unit");
  });
});
