# Testing

## Set up the repo

`scratch` is a homebrew implementation of a subset of git features

```bash
pkg install meow
scratch clone https://github.com/netoneko/meow.git

# switch to a new branch
cd /meow
scratch branch experimental
```

## Running a single non-interactive promt test

```bash
pkg install meow
cd /meow 

meow -m qwen3:8b "read document in prompts/000.txt and execute the instructions from the document"

meow -m MFDoom/deepseek-r1-tool-calling:14b "read document in prompts/000.txt and execute the instructions from the document"
```

## Integration with chainlink

```bash
chainlink create new-prompts -d "create 3 new prompts for yourself to test your capabilities, output them to prompts/002.txt prompts/003.txt and promts/004.txt"

meow -m qwen3:8b "read document in prompts/001.txt and execute the instructions from the document"
```

Alternatively

```bash
meow -m qwen3:8b "you have access to chainlink issue tracker, can you list your tasks and read the first one that you need to accomplish"

meow -m MFDoom/deepseek-r1-tool-calling:14b "you have access to chainlink issue tracker, can you list your tasks and read the first one that you need to accomplish"
```
