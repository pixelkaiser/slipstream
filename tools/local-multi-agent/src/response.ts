export async function collectAssistantOutput(chunks: AsyncIterable<string>): Promise<string> {
  let output = "";
  for await (const chunk of chunks) {
    output += chunk;
  }

  if (!output) {
    throw new Error("OpenAI-compatible endpoint returned no assistant content.");
  }

  return output;
}
