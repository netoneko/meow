# Testing

## Running a single non-interactive promt test

```bash
pkg install meow
cd /meow 

meow -m qwen3:8b "read document in docs/promts/000.txt and execute the instructions from the document"

meow -m MFDoom/deepseek-r1-tool-calling:14b "read document in docs/promts/000.txt and execute the instructions from the document"
```
