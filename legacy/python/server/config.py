from pathlib import Path

PORT = 19820
HOST = "127.0.0.1"
DATA_DIR = Path(__file__).parent / "data"
DB_PATH = DATA_DIR / "chat_memory.db"
