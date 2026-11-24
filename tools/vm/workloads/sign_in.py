#!/usr/bin/env python3
"""sign_in: prints the sign-in of the Google Drive test account, for a
pipe into the guest. Runs on the host.

  sign_in.py          the token that `vapor auth login --token -` reads
  sign_in.py probe    the same with the OAuth client of the build, for
                      `drive_probe.py`

It reads the gitignored `.env` of the checkout: `VAPOR_E2E_GDRIVE_TOKEN`
(the JSON document that Vapor stores for a sign-in) and, for `probe`,
`VAPOR_GDRIVE_CLIENT_ID` and `VAPOR_GDRIVE_CLIENT_SECRET`. The output is
a secret: the script refuses a terminal, and nothing but a pipe to the
guest takes it. Only for the dedicated test account.

  tools/vm/workloads/sign_in.py | STDIN=1 tools/vm/workloads/in-guest.sh 'vapor auth login --token -'
"""
import json
import os
import sys

if sys.stdout.isatty():
    sys.exit("sign_in: the output is a secret; pipe it to the guest")
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..")
env = {}
try:
    with open(os.path.join(ROOT, ".env")) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            key, value = key.replace("export ", "").strip(), value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
                value = value[1:-1]
            env[key] = value
except OSError as error:
    sys.exit(f"sign_in: cannot read .env: {error}")
missing = [key for key in ["VAPOR_E2E_GDRIVE_TOKEN"] + (["VAPOR_GDRIVE_CLIENT_ID", "VAPOR_GDRIVE_CLIENT_SECRET"] if sys.argv[1:] == ["probe"] else []) if not env.get(key)]
if missing:
    sys.exit(f"sign_in: .env does not set {', '.join(missing)}")
sign_in = json.loads(env["VAPOR_E2E_GDRIVE_TOKEN"])
if sys.argv[1:] == ["probe"]:
    sign_in["clientId"] = env["VAPOR_GDRIVE_CLIENT_ID"]
    sign_in["clientSecret"] = env["VAPOR_GDRIVE_CLIENT_SECRET"]
sys.stdout.write(json.dumps(sign_in))
