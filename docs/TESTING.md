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
