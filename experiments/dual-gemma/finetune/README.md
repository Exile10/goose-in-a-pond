# FunctionGemma fine-tune pipeline

Fixes the picker's measured failure classes by LoRA-tuning the 270M model
on synthetic data generated from those exact failures. Everything runs on
the MacBook (MLX); the Jetson never trains.

## The loop

```
gen_data.py     failure-class training data + held-out labelled eval
picker_eval.py  scores any picker endpoint on the held-out set
train.sh        MLX LoRA on the full-precision HF weights
convert.sh      fuse -> GGUF f16 -> Q4_K_M, ready for llama-server
```

```bash
python3 gen_data.py                                   # data/{train,valid,picker_eval}.jsonl
python3 picker_eval.py --label base-q4                # baseline against :8092
bash train.sh                                         # needs HF licence + login, see below
bash convert.sh
llama-server -m runs/functiongemma-270m-tuned-Q4_K_M.gguf --port 8094 -c 2048 -ngl 99 &
python3 picker_eval.py --port 8094 --label tuned      # the before/after number
```

The final verdict is the full-loop eval with the tuned picker:

```bash
PICKER_GGUF=$PWD/runs/functiongemma-270m-tuned-Q4_K_M.gguf PICKER_PORT=8094 ../serve.sh start
cd .. && python3 eval.py --arm all
```

## Gated weights -- the one manual step

The Q4 GGUF we serve cannot be fine-tuned (quantised, lossy). Training
needs the original weights, and `google/functiongemma-270m-it` is
licence-gated: repo metadata reads anonymously, the files 401. Once per
machine:

1. Accept the licence: https://huggingface.co/google/functiongemma-270m-it
2. `.venv/bin/hf auth login` (paste a read token from
   https://huggingface.co/settings/tokens)

`train.sh` then fetches the weights itself.

## What the data teaches

Each generator targets a behaviour measured broken in the parent
experiment (baseline on the held-out set: **12/18, 67%**):

| class | measured failure | share |
| --- | --- | --- |
| stop | babbles to the token cap after the calls, every time | every example ends `<end_of_turn>` |
| register | `set_timer(600, standup)` -> `seconds: 16400`; `seconds=600` -> `100` | 1 in 3 instructions rendered as pseudo-code |
| fidelity | invents numbers instead of copying | varied 1-5 digit values everywhere |
| near-miss | `get_time` request -> `read_note{name:get_time}` | dedicated generator |
| exact-name | "my jetson note" -> `name: my_son_note` | dedicated generator |
| fan-out | second intent's args blended into the first | 30% compound, 2-3 calls |

Half the training examples use procedurally generated synthetic tools so
the behaviours generalise past this bench's eight; the held-out eval is
entirely real-tool and contains the verbatim failures from the loop runs.
`data/picker_eval.jsonl` is never trained on.

## Honest limits

- LoRA on 2k synthetic examples shapes behaviour; it does not add
  knowledge. Expect the register/stop/name classes to move a lot and
  genuinely ambiguous picks to move less.
- Training on one toolbox risks overfitting to it; the synthetic-tool half
  and the never-trained eval set are the guard. If tuned wins here but
  loses on fresh tools, that guard failed -- widen the synthetic pool.
- The tuned model must be A/B'd at the same quant (Q4_K_M) it will serve
  at; convert.sh produces exactly that.
