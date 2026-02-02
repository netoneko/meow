# Testing

## Set up the repo

```bash
pkg install meow
scratch clone https://github.com/netoneko/meow.git
```

## Running a single non-interactive promt test

```bash
pkg install meow
cd /meow 

meow -m qwen3:8b "read document in docs/promts/000.txt and execute the instructions from the document"

meow -m MFDoom/deepseek-r1-tool-calling:14b "read document in docs/promts/000.txt and execute the instructions from the document"
```
