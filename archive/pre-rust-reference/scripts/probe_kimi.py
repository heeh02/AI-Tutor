"""Run real harmless connectivity and synthetic no-tools canary checks."""
import asyncio
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))
from ai_tutor.kimi import KimiProvider

async def main():
    provider = KimiProvider(ROOT)
    try:
        result = await provider.verify_real()
        print(json.dumps(result, ensure_ascii=False, indent=2))
    finally:
        await provider.close()

if __name__ == "__main__":
    asyncio.run(main())
