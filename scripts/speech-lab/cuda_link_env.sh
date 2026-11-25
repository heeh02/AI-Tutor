# Source after task_env is selected. CUDA wheels use lib/ and versioned names,
# while FlashInfer's linker asks for -lcudart and -lcuda under lib64/.
export CUDA_HOME="$task_env/lib/python3.12/site-packages/nvidia/cu13"
export PATH="$CUDA_HOME/bin:$task_env/bin:$PATH"
task_cuda_links="$task_env/.tutor-cuda-link"
mkdir -p "$task_cuda_links"
ln -sfn "$CUDA_HOME/lib/libcudart.so.13" "$task_cuda_links/libcudart.so"
ln -sfn /usr/lib/wsl/lib/libcuda.so "$task_cuda_links/libcuda.so"
export LIBRARY_PATH="$task_cuda_links:$CUDA_HOME/lib:/usr/lib/wsl/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export LD_LIBRARY_PATH="$CUDA_HOME/lib:/usr/lib/wsl/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
