---
name: automate-repeated-work
description: >-
  How to spot repeated work, decide whether to turn it into a helper script or a skill,
  and build it if so. Always use this when you have typed the same multi-step command
  several times, got the same invocation wrong twice, were told "doing this every time
  is tedious", face a long routine setup or build procedure, or see several subagents
  independently writing the same helper. Also use it at a natural stopping point to
  look back on whether anything was repeated. Covers what to do when a needed
  interpreter or tool is missing (ask the user).
---

# Turn repetition into tools

Rebuilding the same procedure by hand is not only slow, it **goes wrong**, and it
goes wrong quietly and differently each time. On the other hand, building a tool
you did not need is pure waste and becomes maintenance debt later. This skill is for
deciding "build / don't build" from signals, and, once you decide to build, building
something that does not break easily.

## Signals to build

Base the decision on observable facts, not speculation.

1. **You typed the same multi-step command three or more times.** Count variants
   that differ only slightly in arguments.
2. **You got the same invocation wrong twice or more.** This is the strongest signal.
   Making mistakes means part of the procedure does not fit in memory, and you will
   get it wrong again.
3. **Several subagents independently wrote the same helper.** Write it once and hand
   it over, and everyone skips that work.
4. **There is an invariant with no feedback.** Forgetting it changes nothing now but
   bites later (adding to an index, appending to a registry file, post-processing).
   Scripts often cannot catch these; then it is a job for prose (docs or a skill).

## Deciding not to build is more common

Do not build if any of these apply.

- **Work done only once.** "It might come up again" is not a reason. Wait until it
  actually comes up a second time.
- **Something you can derive correctly every time.** If you always get there without
  a write-up, writing it down only adds reading effort. When in doubt, try it without
  the tool and see whether it really fails. If it does not fail, you do not need it.
- **A procedure whose shape is not settled yet.** You will fix it every time it
  changes, and stale descriptions left behind do harm.

**A script works even if nobody reads it; prose only works if it is read.** If the
same content can be expressed either way, choose a script. Use prose only for things
that need judgment and cannot be automated.

## What to build

| Form | Good for |
| :--- | :--- |
| Script | Fixed procedures, invocations that are easy to assemble wrongly, environment-dependent preprocessing |
| Skill | Things that need judgment, conventions that cannot be automated, ordering of work |
| Document | Long background explanations, details only needed when starting a particular task |

When in doubt, prefer a script. Skills and documents only take effect when read, so
their yield is worse.

## When building a script

Check in this order. Skipping steps means leaving behind a tool that does not work.

1. **Run it by hand twice and confirm it really has the same shape.** Do not force
   different things into one.
2. **List the mistakes you made and absorb them into the script.** The places you got
   wrong become the specification: working directory, temp file extensions, stdin
   handling (left open, it hangs), and so on.
3. **Build in warnings for predictable pitfalls.** Rather than silently returning a
   stale result, make it say that it is stale.
4. **Actually exercise every way of calling it.** With arguments, with a file, with
   stdin, `--help`. Assume any path you did not exercise is broken.
5. **Decide its place and entry point.** Put it where the repository's conventions say,
   and link it with one line from existing docs (AGENTS.md, README, an index).
   **A tool that nothing references might as well not exist.**

At the top of the script itself, write what it is for, what it looks at, and what it
does not look at. Keeping it usable by a human reader makes it survive tool changes.

## When the tool you want is not available

**Do not perform acrobatics with what you have; ask the user.** They may well say you
can use it.

For example: I once tried to use `python3` for a multi-line text replacement, found it
was not installed, switched to a perl one-liner, tangled the quoting and made two
syntax errors. Afterwards the user said "you don't have to force it; if there is a
language you want, just ask", added it to the environment definition, and that solved
it. All the time spent on acrobatics was wasted.

When asking:

- **Say concretely what it is for and why what you have is not enough.** Mention failed
  attempts if any.
- **Find where the environment is defined first.** `shell.nix` / `flake.nix` /
  `package.json` / `Dockerfile` / `requirements.txt` etc. Pointing at where to add it
  speeds up the decision.
- **After it is added, also fix documents that copy it.** Documents that paste the
  environment definition into their text go stale.

Also, **for an intricate multi-line replacement within one file, the editor's edit tool
is more reliable than writing a script.** You really need a scripting language only for
mechanical bulk processing across many files.

## After building

- Actually use it and check that the effort you expected to save was saved. If not,
  delete it.
- If you made a skill or document, question whether **results really differ compared to
  not having it**. If they do not, the model can derive the content every time, and it
  is not worth keeping.
