$classes = @(
    "Drug","Gene","Protein","Disease","Cancer","Metabolite","Pathway",
    "SignalingPathway","MetabolicPathway","MiRNA","Enzyme","Kinase",
    "KinaseInhibitor","TranscriptionFactor","ReceptorTyrosineKinase",
    "Transporter","Lipid","ProteinComplex","ProteinStructure",
    "StructuralDomain","StructuralProtein","BindingSite","GeneOntology",
    "BioAssay","Variant","BiologicalEntity",
    "interacts_with","metabolizes","regulates","causes","treats",
    "targets","targets_protein","participates_in","catalyzes",
    "annotated_with","associated_with","member_of","has_structure",
    "has_bioactivity","structure_drug","structure_disease"
)

foreach ($c in $classes) {
    $sql = "SELECT COUNT(*) as cnt FROM `"sembio.$c`""
    $body = @{ query = $sql } | ConvertTo-Json
    try {
        $resp = Invoke-WebRequest -Uri http://127.0.0.1:7912/api/query -Method POST -Body $body -ContentType "application/json" -UseBasicParsing
        $json = $resp.Content | ConvertFrom-Json
        if ($json.success) {
            $cnt = $json.data[0].cnt
            Write-Output "sembio.$c : $cnt"
        } else {
            Write-Output "sembio.$c : QUERY_ERROR - $($json.error)"
        }
    } catch {
        $errMsg = $_.Exception.Message
        Write-Output "sembio.$c : ERROR - $errMsg"
    }
}
