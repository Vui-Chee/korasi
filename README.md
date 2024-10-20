# korasi

[![Build status](https://img.shields.io/github/actions/workflow/status/vui-chee/korasi/ci.yml)](https://github.com/vui-chee/korasi/actions)

An AWS client to spin up EC2 instances of various arch to run code that specifically compiles on certain type of hardware. For instance,
if you are a M1 mac user, you may not have a Intel machine to run AVX intrinsics. Similarly, if you want to run Cuda code, and
don't want to spend $3000 just to obtain an Nvidia GPU.

The goal is to run locally written code easily on remote hardware and get back the results (stdout). At the same time, not burn a giant
hole in your wallet just to test certain types of code. On top of that, you own the entire infrastructure, meaning you pay for what you
use.

Right now, the tool is not fully mature enough, as there are quite a number of kinks I still need to work out just to get
user experience right.

**You have been warned. Use at your own risk.**
