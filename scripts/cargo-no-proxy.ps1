$ErrorActionPreference = "Stop"
Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY, Env:ALL_PROXY -ErrorAction SilentlyContinue
Remove-Item Env:http_proxy, Env:https_proxy, Env:all_proxy -ErrorAction SilentlyContinue
& cargo @args
