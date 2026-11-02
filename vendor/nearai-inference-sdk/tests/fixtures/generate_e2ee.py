# Regenerate with: uv run --with nearai-inference-sdk==0.1.0 rs/tests/fixtures/generate_e2ee.py
# The recipient key is fixed for testing; fresh ephemeral keys/nonces change ciphertexts.
import json
from pathlib import Path
from nacl.signing import SigningKey
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from nearai_inference_sdk.core.e2ee import encrypt_e2ee_text
from nearai_inference_sdk import E2eeModelKey
cases=[]
for algo in ['ed25519','ecdsa']:
    seed=bytes([7])*32
    if algo=='ed25519': pub=bytes(SigningKey(seed).verify_key).hex()
    else:
        priv=ec.derive_private_key(int.from_bytes(seed,'big'),ec.SECP256K1())
        pub=priv.public_key().public_bytes(Encoding.X962,PublicFormat.UncompressedPoint)[1:].hex()
    plain='Hello 🌍 — exact UTF-8'
    cases.append(dict(algorithm=algo,private_seed=seed.hex(),public_key=pub,plaintext=plain,ciphertext=encrypt_e2ee_text(plain,E2eeModelKey(signing_algo=algo,public_key=pub))))
path=Path(__file__).with_name('e2ee-python.json')
path.parent.mkdir(exist_ok=True)
path.write_text(json.dumps(dict(source='nearai-inference-sdk Python 0.1.0',cases=cases),ensure_ascii=False,indent=2)+'\n')
